"""使用真实截图测试 mash_cv 的集成流程。

使用方式：
1. 将截图 PNG 放入 tests/test_data/screenshots/。
2. 将独立 find_element 用例的模板放入 tests/test_data/templates/。
3. 编辑 tests/test_data/cases.json 定义预期结果。
4. 运行：poetry run pytest tests/test_integration.py -v

cases.json schema：
{
  "detect": [
    { "image": "screenshots/team_confirm.png", "expectedScreen": "TeamConfirm" }
  ],
  "findElement": [
    {
      "image": "screenshots/team_confirm.png",
      "templateKey": "example_button",
      "region": { "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0 },
      "threshold": 0.8,
      "expectedFound": true
    }
  ]
}

- `image` 路径相对于 test_data/。
- `templateKey` 必须匹配 test_data/templates/ 中不含 .png 的文件名。
- `region` 和 `threshold` 可省略，默认值分别为完整图片和 0.8。
"""

import json
import os
import subprocess
import sys

import pytest

DATA_DIR = os.path.join(os.path.dirname(__file__), "test_data")
CASES_FILE = os.path.join(DATA_DIR, "cases.json")
SIDECAR_DIR = os.path.dirname(os.path.dirname(__file__))
TEMPLATES_DIR = os.path.join(DATA_DIR, "templates")
SERVERS_DIR = os.path.abspath(
    os.path.join(SIDECAR_DIR, "..", "..", "src-tauri", "resources", "servers")
)
SHARED_TEMPLATES_DIR = os.path.join(SERVERS_DIR, "shared", "templates")
JP_TEMPLATES_DIR = os.path.join(SERVERS_DIR, "jp", "templates")
SHARED_CONFIG_FILE = os.path.join(SERVERS_DIR, "shared", "cv.json")
JP_CONFIG_FILE = os.path.join(SERVERS_DIR, "jp", "cv.json")
CN_TEMPLATES_DIR = os.path.join(SERVERS_DIR, "cn", "templates")
CN_CONFIG_FILE = os.path.join(SERVERS_DIR, "cn", "cv.json")

# 每个 detect 用例声明其截图来自哪个服务器（默认 jp）；生产客户端
# 按服务器加载 templates + cv.json，这里保持同样的加载顺序。
SERVER_ASSETS = {
    "jp": (JP_TEMPLATES_DIR, JP_CONFIG_FILE),
    "cn": (CN_TEMPLATES_DIR, CN_CONFIG_FILE),
}


def _load_cases() -> dict:
    if not os.path.isfile(CASES_FILE):
        return {}
    with open(CASES_FILE) as f:
        return json.load(f)


def _resolve(rel_path: str) -> str:
    return os.path.join(DATA_DIR, rel_path)


def _image_exists(rel_path: str) -> bool:
    return os.path.isfile(_resolve(rel_path))


def _has_templates() -> bool:
    if not os.path.isdir(TEMPLATES_DIR):
        return False
    return any(f.endswith(".png") for f in os.listdir(TEMPLATES_DIR))


def _production_load_commands(server: str = "jp") -> list[dict]:
    templates_dir, config_file = SERVER_ASSETS[server]
    return [
        {
            "cmd": "load_templates",
            "dir": SHARED_TEMPLATES_DIR,
            "keyPrefix": "shared",
        },
        {"cmd": "load_templates", "dir": templates_dir, "append": True},
        {"cmd": "load_config", "path": SHARED_CONFIG_FILE},
        {"cmd": "load_config", "path": config_file, "merge": True},
    ]


def _run_commands(commands: list[dict], timeout: int = 30) -> list[dict]:
    stdin = "\n".join(json.dumps(c) for c in commands) + "\n"
    proc = subprocess.run(
        [sys.executable, "-m", "mash_cv"],
        cwd=SIDECAR_DIR,
        input=stdin,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    lines = [l for l in proc.stdout.strip().splitlines() if l]
    return [json.loads(l) for l in lines]


# ── 页面识别测试 ───────────────────────────────────────────────────────


def _detect_cases():
    cases = _load_cases().get("detect", [])
    out = []
    for c in cases:
        img = c["image"]
        expected = c["expectedScreen"]
        server = c.get("server", "jp")
        label = f"{os.path.basename(img)}→{expected}"
        out.append(pytest.param(img, expected, server, id=label))
    return out


@pytest.mark.parametrize("image,expected_screen,server", _detect_cases())
def test_detect_screen(image, expected_screen, server):
    if not _image_exists(image):
        pytest.skip(f"image not found: {image}")
    prefix = _production_load_commands(server)
    if server == "cn":
        # 生产客户端在加载配置后切换 OCR 语言；CN 屏幕的关键词探测依赖它。
        prefix.append({"cmd": "set_server", "server": "CN"})
    # 关键词探测会拉起 OCR 子进程，冷缓存下模型加载可达数十秒。
    responses = _run_commands(prefix + [
        {"cmd": "detect", "imagePath": _resolve(image)},
        {"cmd": "quit"},
    ], timeout=120)
    assert len(responses) == len(prefix) + 1
    for i, resp in enumerate(responses[:-1]):
        assert resp.get("ok") is True, f"command {prefix[i]['cmd']} failed: {resp}"
    assert responses[-1]["screen"] == expected_screen, (
        f"expected {expected_screen}, got {responses[-1]}"
    )


# ── 元素识别测试 ───────────────────────────────────────────────────────


def _find_element_cases():
    cases = _load_cases().get("findElement", [])
    out = []
    for i, c in enumerate(cases):
        img = c["image"]
        key = c["templateKey"]
        label = f"{os.path.basename(img)}+{key}"
        out.append(pytest.param(c, id=label))
    return out


@pytest.mark.parametrize("case", _find_element_cases())
def test_find_element(case):
    image = case["image"]
    if not _image_exists(image):
        pytest.skip(f"image not found: {image}")
    if not _has_templates():
        pytest.skip("no templates in test_data/templates/")

    region = case.get("region", {"x": 0, "y": 0, "w": 1, "h": 1})
    threshold = case.get("threshold", 0.8)
    expected_found = case["expectedFound"]

    responses = _run_commands([
        {"cmd": "load_templates", "dir": TEMPLATES_DIR},
        {
            "cmd": "find_element",
            "imagePath": _resolve(image),
            "templateKey": case["templateKey"],
            "region": region,
            "threshold": threshold,
        },
        {"cmd": "quit"},
    ])

    assert len(responses) == 2
    load_resp = responses[0]
    assert load_resp.get("ok") is True, f"load_templates failed: {load_resp}"

    find_resp = responses[1]
    assert find_resp["found"] is expected_found, (
        f"expected found={expected_found}, got {find_resp}"
    )

    if expected_found:
        assert 0.0 <= find_resp["x"] <= 1.0
        assert 0.0 <= find_resp["y"] <= 1.0


# ── 完整流程：先识别页面，再查找元素 ───────────────────────────────────


def _pipeline_cases():
    """组合使用同一截图的 detect 与 find_element 用例。"""
    data = _load_cases()
    detect_map = {
        c["image"]: (c["expectedScreen"], c.get("server", "jp"))
        for c in data.get("detect", [])
    }
    find_cases = data.get("findElement", [])

    out = []
    for fc in find_cases:
        img = fc["image"]
        if img in detect_map:
            expected_screen, server = detect_map[img]
            label = f"pipeline:{os.path.basename(img)}+{fc['templateKey']}"
            out.append(pytest.param(img, expected_screen, server, fc, id=label))
    return out


@pytest.mark.parametrize("image,expected_screen,server,find_case", _pipeline_cases())
def test_detect_then_find(image, expected_screen, server, find_case):
    """在单个 sidecar session 中执行 load → detect → find。"""
    if not _image_exists(image):
        pytest.skip(f"image not found: {image}")
    if not _has_templates():
        pytest.skip("no templates in test_data/templates/")

    region = find_case.get("region", {"x": 0, "y": 0, "w": 1, "h": 1})
    threshold = find_case.get("threshold", 0.8)

    prefix = _production_load_commands(server)
    if server == "cn":
        prefix.append({"cmd": "set_server", "server": "CN"})

    responses = _run_commands(prefix + [
        {"cmd": "load_templates", "dir": TEMPLATES_DIR, "append": True},
        {"cmd": "detect", "imagePath": _resolve(image)},
        {
            "cmd": "find_element",
            "imagePath": _resolve(image),
            "templateKey": find_case["templateKey"],
            "region": region,
            "threshold": threshold,
        },
        {"cmd": "quit"},
    ], timeout=120)

    assert len(responses) == len(prefix) + 3
    assert all(resp.get("ok") is True for resp in responses[:-2]), responses

    detect_resp = responses[-2]
    assert detect_resp["screen"] == expected_screen

    find_resp = responses[-1]
    assert find_resp["found"] is find_case["expectedFound"]
