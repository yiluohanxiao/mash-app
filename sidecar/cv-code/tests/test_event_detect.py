"""OCR-keyword screen detection tests.

These exercise the keyword pass added for the event-farm runtime without
loading OCR models: ``_ocr_region`` is replaced with synthetic fragments.
"""

import json
from pathlib import Path

import cv2
import numpy as np
import pytest

import mash_cv
from mash_cv import cv as cv_module

REGION = {"x": 0.0, "y": 0.0, "w": 0.2, "h": 0.15}


@pytest.fixture(autouse=True)
def _reset_config():
    mash_cv._set_config({"screens": {}})
    yield
    mash_cv._set_config({"screens": {}})


def _image() -> np.ndarray:
    return np.zeros((108, 192, 3), dtype=np.uint8)


def _fragment(text: str, confidence: float = 0.9) -> dict:
    return {
        "text": text,
        "region": {"x": 0.0, "y": 0.0, "w": 0.1, "h": 0.1},
        "ocrConfidence": confidence,
    }


def _set_ocr(monkeypatch, fragments: list[dict], calls: list | None = None) -> None:
    def fake_ocr(img, region, scale=1.0):
        if calls is not None:
            calls.append(tuple(sorted(region.items())))
        return {
            "fragments": list(fragments),
            "fullText": "\n".join(fragment["text"] for fragment in fragments),
        }

    monkeypatch.setattr(cv_module, "_ocr_region", fake_ocr)


def test_keyword_screen_matches_any_of(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "EventMap": {
                    "detect": {
                        "keywords": ["管理室"],
                        "region": REGION,
                        "priority": 60,
                    }
                }
            }
        }
    )
    _set_ocr(monkeypatch, [_fragment("管理室")])

    assert mash_cv._detect_screen(_image(), ["EventMap"]) == {
        "screen": "EventMap",
        "score": 0.9,
    }


def test_keyword_respects_min_score(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "EventMap": {
                    "detect": {
                        "keywords": ["管理室"],
                        "region": REGION,
                        "minScore": 0.5,
                    }
                }
            }
        }
    )
    _set_ocr(monkeypatch, [_fragment("管理室", confidence=0.2)])

    assert mash_cv._detect_screen(_image(), ["EventMap"])["screen"] == "Unknown"


def test_keyword_groups_are_and(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "Gate": {
                    "detect": {
                        "keywordGroups": [
                            {"anyOf": ["迦勒底之门"], "region": REGION},
                            {"anyOf": ["关闭"], "region": REGION},
                        ],
                        "priority": 65,
                    }
                }
            }
        }
    )
    _set_ocr(monkeypatch, [_fragment("迦勒底之门")])
    assert mash_cv._detect_screen(_image(), ["Gate"])["screen"] == "Unknown"

    _set_ocr(monkeypatch, [_fragment("迦勒底之门"), _fragment("关闭", 0.8)])
    assert mash_cv._detect_screen(_image(), ["Gate"]) == {
        "screen": "Gate",
        "score": 0.8,
    }


def test_template_pass_wins_before_keywords(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "Battle": {"detect": {"template": "battle"}},
                "EventMap": {"detect": {"keywords": ["管理室"], "region": REGION}},
            }
        }
    )
    monkeypatch.setattr(
        cv_module, "_template_detect_score", lambda *args, **kwargs: 0.9
    )
    _set_ocr(monkeypatch, [_fragment("管理室", confidence=0.99)])

    assert mash_cv._detect_screen(_image())["screen"] == "Battle"


def test_override_replaces_template_hit_when_its_keyword_matches(monkeypatch):
    """A dialog drawn over a page keeps that page's template. The override
    screen's keyword probe must be able to take the frame back."""
    mash_cv._set_config(
        {
            "screens": {
                "SupportSelect": {"detect": {"template": "support_select"}},
                "ListUpdate": {
                    "detect": {
                        "keywords": ["成员列表"],
                        "region": REGION,
                        "overrides": ["SupportSelect"],
                    }
                },
            }
        }
    )
    monkeypatch.setattr(
        cv_module, "_template_detect_score", lambda *args, **kwargs: 0.9
    )
    _set_ocr(monkeypatch, [_fragment("成员列表", confidence=0.7)])

    assert mash_cv._detect_screen(_image()) == {
        "screen": "ListUpdate",
        "score": 0.7,
    }


def test_override_keeps_template_hit_when_its_keyword_misses(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "SupportSelect": {"detect": {"template": "support_select"}},
                "ListUpdate": {
                    "detect": {
                        "keywords": ["成员列表"],
                        "region": REGION,
                        "overrides": ["SupportSelect"],
                    }
                },
            }
        }
    )
    monkeypatch.setattr(
        cv_module, "_template_detect_score", lambda *args, **kwargs: 0.9
    )
    _set_ocr(monkeypatch, [_fragment("助战选择")])

    assert mash_cv._detect_screen(_image()) == {
        "screen": "SupportSelect",
        "score": 0.9,
    }


def test_override_only_rechecks_when_its_screen_is_in_scope(monkeypatch):
    """The override screen is a keyword probe; if the caller did not ask for
    it, the template hit must stand rather than pay for an OCR call."""
    mash_cv._set_config(
        {
            "screens": {
                "SupportSelect": {"detect": {"template": "support_select"}},
                "ListUpdate": {
                    "detect": {
                        "keywords": ["成员列表"],
                        "region": REGION,
                        "overrides": ["SupportSelect"],
                    }
                },
            }
        }
    )
    monkeypatch.setattr(
        cv_module, "_template_detect_score", lambda *args, **kwargs: 0.9
    )
    calls: list = []
    _set_ocr(monkeypatch, [_fragment("成员列表")], calls)

    assert mash_cv._detect_screen(_image(), ["SupportSelect"])["screen"] == (
        "SupportSelect"
    )
    assert calls == []


def test_keyword_ocr_is_cached_per_region(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "EventMap": {"detect": {"keywords": ["管理室"], "region": REGION}},
                "Terminal": {"detect": {"keywords": ["通知"], "region": REGION}},
            }
        }
    )
    calls: list = []
    _set_ocr(
        monkeypatch,
        [_fragment("管理室"), _fragment("通知", confidence=0.7)],
        calls=calls,
    )

    result = mash_cv._detect_screen(_image(), ["EventMap", "Terminal"])

    assert result["screen"] == "EventMap"
    assert len(calls) == 1


def test_keyword_scan_stops_after_higher_priority_match(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "EventMap": {
                    "detect": {"keywords": ["管理室"], "region": REGION, "priority": 60}
                },
                "Busy": {
                    "detect": {
                        "keywords": ["该操作无效"],
                        "region": {"x": 0.5, "y": 0.5, "w": 0.2, "h": 0.1},
                        "priority": 15,
                    }
                },
            }
        }
    )
    calls: list = []
    # The fake reader matches every probe, so only the priority short-circuit
    # can keep the lower-priority screen from being OCR'd.
    _set_ocr(monkeypatch, [_fragment("管理室")], calls=calls)

    assert mash_cv._detect_screen(_image())["screen"] == "EventMap"
    assert len(calls) == 1


def test_keyword_text_normalization():
    assert cv_module._normalize_detect_text("管 理　室") == "管理室"
    assert cv_module._normalize_detect_text("ＡＢＣ") == "ABC"


def test_blank_probe_matches_dark_loading_frame():
    mash_cv._set_config(
        {
            "screens": {
                "Loading": {
                    "detect": {"blank": {"maxBrightness": 45}, "priority": 20}
                }
            }
        }
    )
    assert mash_cv._detect_screen(np.zeros((108, 192, 3), np.uint8)) == {
        "screen": "Loading",
        "score": 1.0,
    }


def test_blank_probe_respects_brightness_bounds():
    mash_cv._set_config(
        {
            "screens": {
                "Loading": {
                    "detect": {
                        "blank": {"minBrightness": 45, "maxBrightness": 90},
                        "priority": 20,
                    }
                }
            }
        }
    )
    assert mash_cv._detect_screen(np.zeros((108, 192, 3), np.uint8))["screen"] == (
        "Unknown"
    )
    assert mash_cv._detect_screen(np.full((108, 192, 3), 60, np.uint8))["screen"] == (
        "Loading"
    )


def test_cn_black_loading_frame_detects_by_brightness():
    """Real capture: FGO's black loading page has no text at all."""
    repo_root = Path(__file__).resolve().parents[3]
    with open(
        repo_root / "src-tauri/resources/servers/cn/cv.json", encoding="utf-8"
    ) as f:
        screens = json.load(f)["screens"]
    mash_cv._set_config({"screens": {"Loading": screens["Loading"]}})
    img = cv2.imread(
        str(Path(__file__).parent / "test_data/screenshots/loading_black_cn.png")
    )
    assert img is not None

    assert mash_cv._detect_screen(img, ["Loading"]) == {
        "screen": "Loading",
        "score": 1.0,
    }


def test_blank_pass_runs_only_after_keywords(monkeypatch):
    mash_cv._set_config(
        {
            "screens": {
                "Loading": {
                    "detect": {"blank": {"maxBrightness": 255}, "priority": 20}
                },
                "EventMap": {
                    "detect": {
                        "keywords": ["管理室"],
                        "region": REGION,
                        "priority": 60,
                    }
                },
            }
        }
    )
    _set_ocr(monkeypatch, [_fragment("管理室")])

    assert mash_cv._detect_screen(np.zeros((108, 192, 3), np.uint8))["screen"] == (
        "EventMap"
    )
