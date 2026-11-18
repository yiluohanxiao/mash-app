"""Event-map label and quest-card parsing tests.

The OCR reader is injected, so these run without an OCR model. Real
frames are covered by ``test_event_corpus.py``.
"""

import numpy as np
import pytest

from mash_cv import event_map

FRAME = np.zeros((1080, 1920, 3), dtype=np.uint8)


def fragment(text, x, y, w, h, confidence=0.9):
    return {
        "text": text,
        "region": {"x": x, "y": y, "w": w, "h": h},
        "ocrConfidence": confidence,
    }


def reader(*fragments):
    return lambda img, region: {
        "fragments": list(fragments),
        "fullText": "\n".join(item["text"] for item in fragments),
    }


def test_label_filter_keeps_place_names_and_rejects_ui_text():
    assert event_map._is_label("仙女座")
    assert event_map._is_label("废弃教堂遗迹")
    for rejected in [
        "管理室",
        "活动点数",
        "获得的活动点数",
        "推荐等级",
        "等级90",
        "AP40",
        "区域A",
        "关闭",
        "仙女座遗迹区域名称太长",
        "短",
        "已获得的活动点数",
    ]:
        assert not event_map._is_label(rejected), rejected


def test_map_labels_drop_edge_cut_and_non_label_fragments():
    result = event_map.read_map_labels(
        FRAME,
        reader(
            fragment("仙女座", 0.30, 0.20, 0.10, 0.04),
            fragment("废弃教堂", 0.60, 0.50, 0.12, 0.04),
            fragment("管理室", 0.10, 0.10, 0.08, 0.04),
            fragment("切一半", 0.0, 0.30, 0.03, 0.04),
            fragment("右边切", 0.97, 0.30, 0.03, 0.04),
        ),
    )

    assert [label["text"] for label in result["labels"]] == ["仙女座", "废弃教堂"]
    first = result["labels"][0]
    assert first["cx"] == pytest.approx(0.35)
    assert first["cy"] == pytest.approx(0.22)
    assert first["x1"] == pytest.approx(0.40)
    assert first["confidence"] == 0.9


def test_card_parse_keeps_level_suffix_and_filters_english_loot_names():
    card = event_map.parse_card(
        [
            fragment("Trick or Treat", 0.53, 0.19, 0.20, 0.03),
            fragment("居住于M星的奥林波斯诸神", 0.53, 0.20, 0.32, 0.04),
            fragment("推荐", 0.53, 0.27, 0.05, 0.03),
            fragment("等级90++", 0.53, 0.28, 0.10, 0.03),
            fragment("AP40", 0.53, 0.32, 0.06, 0.03),
        ]
    )

    assert card["found"] is True
    assert card["name"] == "居住于M星的奥林波斯诸神"
    assert (card["level"], card["plus"], card["ap"]) == (90, 2, 40)
    assert card["locked"] is False


def test_card_parse_marks_lock_conditions_and_threshold():
    card = event_map.parse_card(
        [
            fragment("主线关卡第五节", 0.53, 0.20, 0.20, 0.04),
            fragment("开放条件：点数达 12,000 以上", 0.53, 0.30, 0.30, 0.03),
            fragment("AP0", 0.53, 0.34, 0.06, 0.03),
        ]
    )

    assert card["locked"] is True
    assert card["ap"] == 0
    assert event_map.card_threshold(card["cond"]) == 12000
    assert event_map.card_threshold("无限制") == 0


def test_card_parse_requires_a_name():
    card = event_map.parse_card([fragment("等级90", 0.53, 0.28, 0.10, 0.03)])
    assert card["found"] is False
    assert card["name"] == ""


def test_points_prefers_the_fixed_box():
    result = event_map.read_event_points(
        FRAME, reader(fragment("歌姬点数 44,400", 0.05, 0.25, 0.08, 0.04))
    )
    assert result["points"] == 44400


def test_points_accepts_a_label_and_number_in_one_fragment():
    result = event_map.read_event_points(
        FRAME, reader(fragment("活动点数 1,200", 0.03, 0.20, 0.15, 0.04))
    )
    assert result["points"] == 1200


def test_points_falls_back_to_the_number_below_the_label():
    fragments = [
        fragment("活动点数", 0.03, 0.20, 0.08, 0.03),
        fragment("88,000", 0.03, 0.24, 0.08, 0.03),
        fragment("12", 0.03, 0.60, 0.02, 0.03),
    ]

    def read(img, region):
        # The fixed counter box is empty; only the wide search finds it.
        items = [] if region is event_map.POINTS_BOX else fragments
        return {
            "fragments": list(items),
            "fullText": "\n".join(item["text"] for item in items),
        }

    result = event_map.read_event_points(FRAME, read)
    assert result["points"] == 88000


def test_points_returns_none_when_nothing_is_readable():
    assert event_map.read_event_points(FRAME, reader())["points"] is None
