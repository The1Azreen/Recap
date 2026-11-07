"""Local web UI: paste a link or drop a file, watch progress, browse and search past recaps."""

from __future__ import annotations

import shutil
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse, PlainTextResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from .. import config, export, library, pipeline
from ..config import RecapError
from ..summarize import STYLES, ask, summarize

STATIC = Path(__file__).parent / "static"
app = FastAPI(title="Recap")
app.mount("/static", StaticFiles(directory=STATIC), name="static")

# One job at a time: transcription saturates the GPU anyway.
_pool = ThreadPoolExecutor(max_workers=1)
_jobs: dict[str, dict] = {}
_lock = threading.Lock()


def _update(job_id: str, **kw) -> None:
    with _lock:
        _jobs[job_id].update(kw, updated=time.time())


def _work(job_id: str, source: str, style: str, do_summary: bool, transcribe: bool, uploaded: bool):
    def progress(stage, frac, msg):
        _update(job_id, stage=stage, progress=frac, message=msg)

    _update(job_id, status="running", message="Starting")
    try:
        item_id = pipeline.run(source, style=style, do_summary=do_summary, force_transcribe=transcribe,
                               move_media=uploaded, progress=progress)
        _update(job_id, status="done", item_id=item_id, progress=1.0, message="Done")
    except RecapError as e:
        _update(job_id, status="error", message=str(e))
    except Exception as e:  # show anything unexpected rather than hanging the UI
        _update(job_id, status="error", message=f"{type(e).__name__}: {e}")
    finally:
        if uploaded:
            shutil.rmtree(Path(source).parent, ignore_errors=True)


def _err(e: RecapError):
    raise HTTPException(404 if "No recap" in str(e) else 400, str(e))


@app.get("/")
def index():
    return FileResponse(STATIC / "index.html")


@app.get("/api/system")
def system():
    cli = config.whisper_cli()
    if cli and config.whisper_cli_has_vulkan(cli):
        engine = "whisper.cpp · Vulkan GPU"
    elif cli:
        engine = "whisper.cpp · CPU"
    else:
        try:
            import faster_whisper  # noqa: F401
            engine = "faster-whisper · CPU"
        except ImportError:
            engine = "no transcriber"
    import os
    return {"engine": engine, "gpu": bool(cli and config.whisper_cli_has_vulkan(cli)),
            "styles": list(STYLES), "claude_model": config.CLAUDE_MODEL,
            "has_key": bool(os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("ANTHROPIC_AUTH_TOKEN"))}


@app.post("/api/jobs")
async def create_job(url: str = Form(""), style: str = Form("general"), summary: bool = Form(True),
                     transcribe: bool = Form(False), file: UploadFile | None = File(None)):
    if style not in STYLES:
        raise HTTPException(400, f"Unknown style {style}")
    uploaded = False
    if file is not None and file.filename:
        name = Path(file.filename).name
        # Own folder per upload so the file keeps its real name (which becomes the title).
        dest = config.UPLOADS_DIR / uuid.uuid4().hex[:12] / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        with open(dest, "wb") as f:
            shutil.copyfileobj(file.file, f, 1 << 20)
        source, uploaded, label = str(dest), True, name
    elif url.strip():
        source = label = url.strip()
    else:
        raise HTTPException(400, "Paste a link or choose a file.")

    job_id = uuid.uuid4().hex[:12]
    with _lock:
        _jobs[job_id] = {"id": job_id, "label": label, "status": "queued", "stage": "queued",
                         "progress": None, "message": "Queued", "item_id": None, "updated": time.time()}
    _pool.submit(_work, job_id, source, style, summary, transcribe, uploaded)
    return _jobs[job_id]


@app.get("/api/jobs/{job_id}")
def job(job_id: str):
    with _lock:
        if job_id not in _jobs:
            raise HTTPException(404, "No such job")
        return dict(_jobs[job_id])


@app.get("/api/library")
def items():
    return library.list_items()


@app.get("/api/library/{item_id}")
def item(item_id: str):
    try:
        return {"meta": library.meta(item_id), "transcript": library.transcript(item_id).to_dict(),
                "summary": library.summary(item_id)}
    except RecapError as e:
        _err(e)


@app.delete("/api/library/{item_id}")
def delete_item(item_id: str):
    try:
        library.delete(item_id)
    except RecapError as e:
        _err(e)
    return {"ok": True}


@app.get("/api/library/{item_id}/media")
def media(item_id: str):
    try:
        p = library.media_path(item_id)
    except RecapError as e:
        _err(e)
    if not p or not p.is_file():
        raise HTTPException(404, "No media saved for this recap")
    return FileResponse(p)


@app.get("/api/library/{item_id}/export/{fmt}")
def export_item(item_id: str, fmt: str):
    if fmt not in export.FORMATS:
        raise HTTPException(400, "Unknown format")
    try:
        t, s, m = library.transcript(item_id), library.summary(item_id), library.meta(item_id)
    except RecapError as e:
        _err(e)
    body = export.render(fmt, t, s)
    return PlainTextResponse(body, media_type="text/plain; charset=utf-8", headers={
        "Content-Disposition": f'attachment; filename="{item_id}.{fmt}"'})


class AskBody(BaseModel):
    question: str


@app.post("/api/library/{item_id}/ask")
def ask_item(item_id: str, body: AskBody):
    try:
        return {"answer": ask(library.transcript(item_id), body.question)}
    except RecapError as e:
        _err(e)


class SummaryBody(BaseModel):
    style: str = "general"


@app.post("/api/library/{item_id}/summarize")
def resummarize(item_id: str, body: SummaryBody):
    try:
        s = summarize(library.transcript(item_id), body.style)
        library.set_summary(item_id, s, body.style)
        return {"summary": s, "meta": library.meta(item_id)}
    except RecapError as e:
        _err(e)


@app.get("/api/search")
def search(q: str):
    return library.search(q)
