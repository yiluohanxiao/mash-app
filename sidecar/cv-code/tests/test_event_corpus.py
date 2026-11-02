"""Corpus regression for event-farm screen detection.

Runs the real CN screen config against the fgo-auto capture corpus under
``reference/fgo-auto/tests/corpus`` when that read-only export is present.
The export is git-ignored (110 MB), so this module skips on any checkout
without it; the synthetic cases in ``test_event_detect.py`` still run.

Runtime is roughly 2 minutes: every positive frame runs a handful of
small OCR probes through the disposable OCR worker.
"""

from __future__ import annotations

import json
from pathlib import Path

import cv2
import pytest

import mash_cv
from mash_cv import cv as cv_module

REPO_ROOT = Path(__file__).resolve().parents[3]
REFERENCE = REPO_ROOT / "reference" / "fgo-auto"
CORPUS_DIR = REFERENCE / "tests" / "corpus"
LABELS_PATH = REFERENCE / "tests" / "labels.json"
CN_RESOURCES = REPO_ROOT / "src-tauri" / "resources" / "servers" / "cn"
SHARED_RESOURCES = REPO_ROOT / "src-tauri" / "resources" / "servers" / "shared"

# fgo-auto state -> mash Screen, for the screens the M0 detector implements.
POSITIVE_STATES = {
    "event_map": "EventMap",
    "terminal": "Terminal",
    "node_detail": "NodeDetail",
    "quest_confirm": "QuestConfirm",
    "story": "Story",
    "skip_confirm": "SkipConfirm",
    "tap_continue": "TapContinue",
    "loading": "Loading",
    "title": "Title",
    "defeat": "Defeat",
    "busy": "Busy",
    # M1 screens with corpus samples.
    "reward_page": "RewardPage",
    "reward_select": "RewardSelect",
    "gate": "Gate",
    "party_confirm": "PartyConfirm",
    "list_update": "ListUpdate",
}

# Screens the event runtime may observe. ``servant_full``, ``repeat_confirm``
# and ``tip`` have no corpus sample yet, but their probes still participate
# in the priority contract.
EVENT_SCREENS = set(POSITIVE_STATES.values()) | {
    "Cg",
    "ServantFull",
    "RepeatConfirm",
    "Tip",
}

# Frames whose fgo-auto label depends on a screen this milestone does not
# implement yet. ``59_connecting`` is the white "连接中" page: it has no
# "加载中" text, so mash observes it as Unknown. The event loop waits on
# Unknown exactly like on Loading, so behaviour is unchanged; the label
# returns once the brightness rule lands.
KNOWN_GAPS = {"59_connecting.png"}

# Mirrors ``EVENT_DETECTION_SCREENS`` on the Rust side: the battle scope the
# runner already uses plus the event-only screens.
BATTLE_SCOPE = [
    "RankUpQuest",
    "TeamConfirm",
    "TeamChange",
    "SupportSelect",
    "ServantSelect",
    "Battle",
    "Attack",
    "BattleResultLoot",
    "BattleResultExp",
    "BattleResultBond",
    "BattleResultContinue",
    "BattleResultFriendRequest",
    "APRecovery",
]
EVENT_SCOPE = BATTLE_SCOPE + [
    "EventMap",
    "Terminal",
    "NodeDetail",
    "QuestConfirm",
    "Story",
    "SkipConfirm",
    "TapContinue",
    "Loading",
    "Title",
    "Defeat",
    "Busy",
    "Cg",
    "RewardSelect",
    "RewardPage",
    "ServantFull",
    "Gate",
    "Tip",
    "PartyConfirm",
    "RepeatConfirm",
    "ListUpdate",
]

pytestmark = pytest.mark.skipif(
    not (CORPUS_DIR.is_dir() and LABELS_PATH.is_file()),
    reason="fgo-auto corpus export is not available in this checkout",
)


@pytest.fixture(scope="module")
def labels() -> dict:
    return _load_labels()


@pytest.fixture(scope="module")
def cn_detector():
    """Load the production CN templates + config and pin the CN OCR model."""
    mash_cv._load_templates(str(SHARED_RESOURCES / "templates"), key_prefix="shared/")
    mash_cv._load_templates(str(CN_RESOURCES / "templates"), append=True)
    mash_cv._load_config(str(SHARED_RESOURCES / "cv.json"))
    mash_cv._load_config(str(CN_RESOURCES / "cv.json"), merge=True)
    previous_server = cv_module._current_server
    cv_module._set_server("CN")
    yield
    cv_module._release_ocr("event-corpus-test-finished")
    # Leave the shared OCR server as it was; later modules assume the
    # default unless they pin their own.
    cv_module._set_server(previous_server)


def _load_labels() -> dict:
    if not LABELS_PATH.is_file():
        return {}
    with open(LABELS_PATH, "r", encoding="utf-8") as f:
        return json.load(f)["screens"]


def _event_cases(labels: dict) -> list[tuple[str, str]]:
    cases = []
    for file_name, state in labels.items():
        expected = POSITIVE_STATES.get(state)
        if expected is None or file_name in KNOWN_GAPS:
            continue
        if not (CORPUS_DIR / file_name).is_file():
            continue
        cases.append((file_name, expected))
    return sorted(cases)


def test_event_screen_priorities_follow_the_safety_order():
    """Priority is the safety contract: defeat beats everything, and the
    map/terminal probes must outrank the generic card/loading probes."""
    with open(CN_RESOURCES / "cv.json", "r", encoding="utf-8") as f:
        screens = json.load(f)["screens"]
    priorities = {
        name: screens[name]["detect"]["priority"]
        for name in EVENT_SCREENS
        if "detect" in screens.get(name, {})
    }
    order = [
        "Defeat",
        "ServantFull",
        "RewardSelect",
        "Title",
        "Gate",
        "EventMap",
        "Terminal",
        "QuestConfirm",
        "Tip",
        "SkipConfirm",
        "Story",
        "PartyConfirm",
        "ListUpdate",
        "RepeatConfirm",
        "RewardPage",
        "TapContinue",
        "NodeDetail",
        "Loading",
        "Busy",
    ]
    for higher, lower in zip(order, order[1:]):
        assert priorities[higher] > priorities[lower], (higher, lower)
    assert "detect" not in screens["Cg"]


@pytest.mark.parametrize(
    ("file_name", "expected"),
    _event_cases(_load_labels()),
)
def test_corpus_frame_detects_as_its_label(
    cn_detector, file_name: str, expected: str
):
    img = cv2.imread(str(CORPUS_DIR / file_name))
    assert img is not None, file_name

    result = mash_cv._detect_screen(img, EVENT_SCOPE)

    assert result["screen"] == expected, (file_name, result)


def test_battle_and_result_frames_never_look_like_event_screens(cn_detector, labels):
    """Negative samples: the event keyword probes must not steal battle or
    settlement frames, where blind taps would waste AP or stones."""
    checked = 0
    for file_name, state in sorted(labels.items()):
        if state not in ("battle", "result"):
            continue
        img = cv2.imread(str(CORPUS_DIR / file_name))
        if img is None:
            continue
        checked += 1
        result = mash_cv._detect_screen(img, EVENT_SCOPE)
        assert result["screen"] not in EVENT_SCREENS, (file_name, result)
    assert checked > 0
