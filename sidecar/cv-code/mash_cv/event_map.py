"""Event-map label and quest-card reading.

Ports the OCR half of fgo-auto's ``nav.py`` to normalized coordinates:
which OCR fragments count as region labels, and how a quest card's
name / recommended level / AP / lock state are parsed out of the card
box. Navigation, selection, and caching stay on the Rust side.

The OCR reader is injected (``read`` = ``cv._ocr_region``) so this
module can be unit-tested without loading an OCR model.
"""

from __future__ import annotations

import re

# Probe boxes in normalized 0..1 coordinates (reference is 1920x1080).
MAP_BAND = {"x": 80 / 1920, "y": 100 / 1080, "w": 1800 / 1920, "h": 730 / 1080}
MAP_EDGE_PX = 40
CARD_BOX = {"x": 1000 / 1920, "y": 150 / 1080, "w": 900 / 1920, "h": 270 / 1080}
NAME_BAND_Y = (185 / 1080, 255 / 1080)
POINTS_BOX = {"x": 95 / 1920, "y": 258 / 1080, "w": 167 / 1920, "h": 60 / 1080}
POINTS_SEARCH = {"x": 40 / 1920, "y": 170 / 1080, "w": 390 / 1920, "h": 300 / 1080}

LABEL_SKIP = (
    "上一个", "上一次", "下一个", "菜单", "活动报酬", "管理室", "返回",
    "已获得的活动点数", "获得的活动点数", "活动点数",
)
CARD_WORDS = (
    "消耗", "任务进行", "首次报酬", "推荐", "已获取", "职阶", "倾向",
    "完成", "关卡举办", "举办期间", "关联从者", "点数", "报酬", "万圣节",
    "圣节", "关闭", "前进", "战斗", "开始", "取消", "决定",
)
_NON_LABEL = re.compile(r"[0-9A-Za-z/：:，,。.！!？?%]")
_LEVEL = re.compile(r"等级\s*(\d+)\s*([+＋]*)")
_AP = re.compile(r"AP\s*(\d+)")
_THRESHOLD = re.compile(r"点数达\s*([\d,，\s]+)\s*以上")


def _is_label(text: str) -> bool:
    """A region label is a 3–10 character pure-CJK place name."""
    text = text.strip()
    if not 3 <= len(text) <= 10:
        return False
    if text in LABEL_SKIP or any(word in text for word in LABEL_SKIP):
        return False
    if any(word in text for word in CARD_WORDS):
        return False
    return _NON_LABEL.search(text) is None


def _has_cjk(text: str) -> bool:
    return any("一" <= ch <= "鿿" for ch in text)


def _parse_int(text: str) -> int:
    digits = re.sub(r"[^\d]", "", text or "")
    return int(digits) if digits else 0


def _rect(fragment: dict) -> dict:
    region = fragment.get("region") or {}
    return {
        "x0": float(region.get("x", 0.0)),
        "y0": float(region.get("y", 0.0)),
        "x1": float(region.get("x", 0.0)) + float(region.get("w", 0.0)),
        "y1": float(region.get("y", 0.0)) + float(region.get("h", 0.0)),
    }


def read_map_labels(img, read) -> dict:
    """Read the complete region labels in the current map viewport.

    Labels cut by the screen edge are dropped: their text is unreliable
    and tapping them opens the wrong card.
    """
    height, width = img.shape[:2]
    result = read(img, MAP_BAND)
    labels = []
    for fragment in result.get("fragments", []):
        text = str(fragment.get("text") or "").strip()
        if not _is_label(text):
            continue
        rect = _rect(fragment)
        if rect["x0"] * width < MAP_EDGE_PX or rect["x1"] * width > width - MAP_EDGE_PX:
            continue
        labels.append(
            {
                "text": text,
                "cx": (rect["x0"] + rect["x1"]) / 2,
                "cy": (rect["y0"] + rect["y1"]) / 2,
                "x0": rect["x0"],
                "y0": rect["y0"],
                "x1": rect["x1"],
                "y1": rect["y1"],
                "confidence": float(fragment.get("ocrConfidence") or 0.0),
            }
        )
    return {"labels": labels, "fullText": result.get("fullText", "")}


def parse_card(fragments: list[dict]) -> dict:
    """Parse one card box's OCR fragments.

    ``plus`` keeps the ``+``/``++`` suffix of the recommended level: the
    highest-tier farming node is ``等级90++``, and dropping it would rank
    it equal to an ordinary level-90 node.
    """
    joined = " ".join(str(fragment.get("text") or "").strip() for fragment in fragments)

    candidates = []
    for fragment in fragments:
        text = str(fragment.get("text") or "").strip()
        rect = _rect(fragment)
        cy = (rect["y0"] + rect["y1"]) / 2
        if not NAME_BAND_Y[0] <= cy <= NAME_BAND_Y[1]:
            continue
        if text in ("推荐", "职阶", "倾向") or text.startswith("等级"):
            continue
        if len(text) < 3 or not _has_cjk(text):
            continue
        candidates.append((rect["x1"] - rect["x0"], text))
    name = max(candidates)[1] if candidates else ""

    level_match = _LEVEL.search(joined)
    level = int(level_match.group(1)) if level_match else 0
    plus = len(level_match.group(2)) if level_match else 0
    ap_match = _AP.search(joined)
    ap = int(ap_match.group(1)) if ap_match else 0
    locked = any(word in joined for word in ("开放", "以上", "编制需符合"))
    return {
        "found": bool(name),
        "name": name,
        "level": level,
        "plus": plus,
        "ap": ap,
        "locked": locked,
        "cond": joined[:220],
    }


def read_quest_card(img, read) -> dict:
    result = read(img, CARD_BOX)
    card = parse_card(result.get("fragments", []))
    card["fullText"] = result.get("fullText", "")
    return card


def card_threshold(cond: str) -> int:
    """Event points required to unlock the card, or 0 when unconstrained."""
    match = _THRESHOLD.search(cond or "")
    return _parse_int(match.group(1)) if match else 0


def read_event_points(img, read) -> dict:
    """Read the event-point counter on the left of the map.

    The label and the number are sometimes split into two fragments, so
    the fallback looks for a number directly below the ``点数`` label.
    """
    direct = read(img, POINTS_BOX)
    value = _first_number(direct.get("fullText", ""), min_digits=1)
    if value is not None:
        return {"points": value, "fullText": direct.get("fullText", "")}

    result = read(img, POINTS_SEARCH)
    fragments = result.get("fragments", [])
    for fragment in fragments:
        text = str(fragment.get("text") or "")
        if "点数" in text:
            value = _first_number(text)
            if value is not None:
                return {"points": value, "fullText": result.get("fullText", "")}

    labels = [
        (fragment, _rect(fragment))
        for fragment in fragments
        if "点数" in str(fragment.get("text") or "")
    ]
    numbers = [
        (fragment, _rect(fragment))
        for fragment in fragments
        if _first_number(str(fragment.get("text") or "")) is not None
        and "点数" not in str(fragment.get("text") or "")
    ]
    for label, label_rect in labels:
        label_cy = (label_rect["y0"] + label_rect["y1"]) / 2
        label_cx = (label_rect["x0"] + label_rect["x1"]) / 2
        below = [
            (fragment, rect)
            for fragment, rect in numbers
            if 0 < (rect["y0"] + rect["y1"]) / 2 - label_cy <= 70 / 1080
            and abs((rect["x0"] + rect["x1"]) / 2 - label_cx) < 180 / 1920
        ]
        if below:
            fragment, _ = min(below, key=lambda item: item[1]["y0"])
            return {
                "points": _first_number(str(fragment.get("text") or "")),
                "fullText": result.get("fullText", ""),
            }
    return {"points": None, "fullText": result.get("fullText", "")}


def _first_number(text: str, min_digits: int = 3) -> int | None:
    match = re.search(r"[\d,，]{%d,}" % min_digits, text or "")
    return _parse_int(match.group(0)) if match else None
