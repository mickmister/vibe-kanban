from __future__ import annotations

import os
import time
from dataclasses import dataclass
from typing import Any

try:
    from mitmproxy import ctx
except ImportError:  # pragma: no cover - allows import without mitmproxy installed
    ctx = None  # type: ignore[assignment]


@dataclass
class FlowState:
    started_at: float
    messages_seen: int = 0
    closed: bool = False


class WebSocketChaos:
    def __init__(self) -> None:
        raw_match = os.getenv("VK_WS_MATCH", "")
        self.match_terms = [term.strip() for term in raw_match.split(",") if term.strip()]
        self.kill_after_messages = int(os.getenv("VK_WS_KILL_AFTER_MESSAGES", "0") or "0")
        self.kill_after_seconds = float(os.getenv("VK_WS_KILL_AFTER_SECONDS", "0") or "0")
        self.delay_seconds = float(os.getenv("VK_WS_DELAY_MS", "0") or "0") / 1000.0
        self.close_mode = os.getenv("VK_WS_CLOSE_MODE", "kill").strip().lower() or "kill"
        self.verbose = os.getenv("VK_WS_LOG", "1") != "0"
        self.flow_state: dict[str, FlowState] = {}

    def websocket_start(self, flow: Any) -> None:
        if not self._matches(flow):
            return

        self.flow_state[flow.id] = FlowState(started_at=time.monotonic())
        self._log(
            "accepted",
            path=self._path(flow),
            kill_after_messages=self.kill_after_messages,
            kill_after_seconds=self.kill_after_seconds,
            delay_ms=int(self.delay_seconds * 1000),
            close_mode=self.close_mode,
        )

    def websocket_message(self, flow: Any) -> None:
        if not self._matches(flow):
            return

        state = self.flow_state.setdefault(flow.id, FlowState(started_at=time.monotonic()))
        state.messages_seen += 1

        if self.delay_seconds > 0:
            time.sleep(self.delay_seconds)

        elapsed = time.monotonic() - state.started_at
        if (
            self.kill_after_messages > 0
            and state.messages_seen >= self.kill_after_messages
        ) or (
            self.kill_after_seconds > 0
            and elapsed >= self.kill_after_seconds
        ):
            self._kill(flow, state, elapsed)

    def websocket_end(self, flow: Any) -> None:
        if flow.id in self.flow_state:
            self._log("ended", path=self._path(flow))
            self.flow_state.pop(flow.id, None)

    def error(self, flow: Any) -> None:
        if flow.id in self.flow_state:
            self._log("flow error", path=self._path(flow), error=getattr(flow.error, "msg", flow.error))
            self.flow_state.pop(flow.id, None)

    def _kill(self, flow: Any, state: FlowState, elapsed: float) -> None:
        if state.closed:
            return

        state.closed = True
        self._log(
            "terminating flow",
            path=self._path(flow),
            messages_seen=state.messages_seen,
            elapsed_seconds=f"{elapsed:.2f}",
            close_mode=self.close_mode,
        )
        websocket = getattr(flow, "websocket", None)
        if self.close_mode == "clean" and websocket is not None:
            close_fn = getattr(websocket, "close", None)
            if callable(close_fn):
                try:
                    close_fn(code=1000, reason="vk-ws-chaos")
                    return
                except TypeError:
                    close_fn()
                    return

        flow.kill()

    def _matches(self, flow: Any) -> bool:
        websocket = getattr(flow, "websocket", None)
        if websocket is None:
            return False

        if not self.match_terms:
            return True

        path = self._path(flow)
        return any(term in path for term in self.match_terms)

    def _path(self, flow: Any) -> str:
        request = getattr(flow, "request", None)
        if request is None:
            return "<unknown>"
        return getattr(request, "path", None) or getattr(request, "pretty_url", "<unknown>")

    def _log(self, message: str, **fields: object) -> None:
        if not self.verbose:
            return

        rendered = " ".join(f"{key}={value}" for key, value in fields.items())
        line = f"[vk-ws-chaos] {message}"
        if rendered:
            line = f"{line} {rendered}"

        if ctx is not None:
            ctx.log.info(line)
        else:
            print(line)


addons = [WebSocketChaos()]
