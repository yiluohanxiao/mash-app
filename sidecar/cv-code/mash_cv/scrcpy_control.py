"""scrcpy 2.7 touch wire format and execution of Rust-scheduled events.

This module has no CV or gesture policy. A request contains one complete
gesture in video coordinates, with offsets from the gesture start. A gesture
may hold several fingers at once (pinch): each event carries a ``pointerId``
and scrcpy converts a second DOWN into ACTION_POINTER_DOWN.
Protocol: Genymobile/scrcpy, tag v2.7, app/src/control_msg.c.
"""

import socket
import struct
import time

# scrcpy's PointersState supports ten concurrent fingers.
MAX_POINTERS = 10


def touch_packet(
    action: int, x: int, y: int, width: int, height: int, pointer_id: int = 0
) -> bytes:
    # type, action, pointer id, x/y, video width/height, pressure, buttons.
    # A nonnegative pointer id represents a finger (not scrcpy's mouse id -1).
    return struct.pack(
        ">BBQiiHHHII", 2, action, pointer_id, x, y, width, height,
        0 if action == 1 else 0xFFFF, 0, 0,
    )


def validate_events(events: list, width: int, height: int) -> None:
    if not (0 < width <= 65535 and 0 < height <= 65535):
        raise ValueError("invalid scrcpy video dimensions")
    if not isinstance(events, list) or not 2 <= len(events) <= 604:
        raise ValueError("expected a complete scrcpy gesture")
    previous = 0
    pressed = set()
    for index, event in enumerate(events):
        if not isinstance(event, dict):
            raise ValueError("invalid scrcpy touch event")
        action, x, y, at_ms = (event.get(key) for key in ("action", "x", "y", "atMs"))
        pointer = event.get("pointerId", 0)
        if any(type(value) is not int for value in (action, x, y, at_ms, pointer)):
            raise ValueError("touch event fields must be integers")
        if action not in (0, 1, 2) or not 0 <= pointer < MAX_POINTERS:
            raise ValueError("invalid scrcpy contact sequence or coordinates")
        if not (0 <= x < width and 0 <= y < height):
            raise ValueError("invalid scrcpy contact sequence or coordinates")
        if index == 0 and action != 0:
            raise ValueError("scrcpy gesture must begin with a pointer down")
        if action == 0:
            if pointer in pressed:
                raise ValueError("duplicate scrcpy pointer down")
            pressed.add(pointer)
        elif pointer not in pressed:
            raise ValueError("scrcpy event for a pointer that is not down")
        elif action == 1:
            pressed.discard(pointer)
        if not previous <= at_ms <= 60_000 or (index == 0 and at_ms != 0):
            raise ValueError("invalid scrcpy touch schedule")
        previous = at_ms
    if pressed:
        raise ValueError("scrcpy gesture left a pointer pressed")


class ScrcpyControl:
    def __init__(self, sock: socket.socket):
        self._sock = sock
        # pointer id -> (x, y, width, height) for every finger still down.
        self._pressed = {}
        self._failed = False

    def send(self, events: list, width: int, height: int) -> None:
        validate_events(events, width, height)  # Validate everything before DOWN.
        if self._failed:
            raise RuntimeError("scrcpy control connection failed; restart the stream")
        deadline = time.monotonic()
        previous_ms = 0
        try:
            for event in events:
                deadline += (event["atMs"] - previous_ms) / 1000
                remaining = deadline - time.monotonic()
                if remaining > 0:
                    time.sleep(remaining)
                pointer = event.get("pointerId", 0)
                point = (event["x"], event["y"], width, height)
                # Record before writing: a failing sendall may have sent DOWN.
                if event["action"] != 1:
                    self._pressed[pointer] = point
                self._sock.sendall(touch_packet(event["action"], *point, pointer))
                if event["action"] == 1:
                    self._pressed.pop(pointer, None)
                # scrcpy timestamps events on receipt. Never burst overdue
                # MOVE events or shorten the stationary tail after a host
                # scheduling stall / slow write; let the gesture run longer.
                deadline = max(deadline, time.monotonic())
                previous_ms = event["atMs"]
        except Exception:
            self._failed = True
            raise
        finally:
            self.release()

    def release(self) -> None:
        pressed, self._pressed = self._pressed, {}
        for pointer, point in pressed.items():
            try:
                self._sock.sendall(touch_packet(1, *point, pointer))
            except OSError:
                pass

    def close(self) -> None:
        self.release()
        self._failed = True
        try:
            self._sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self._sock.close()
