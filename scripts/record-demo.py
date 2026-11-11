"""Record a local exported dashboard. Requires playwright, Chromium and Pillow.
Usage: python scripts/record-demo.py http://localhost:8766
Serve web/ over an exported account/site directory before recording.
"""
import io
import sys
from pathlib import Path
from PIL import Image
from playwright.sync_api import sync_playwright

frames = []
with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page(viewport={"width": 1100, "height": 780}, device_scale_factor=1)
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.goto(sys.argv[1], wait_until="networkidle")
    for tab in ["overview", "desk", "holdings", "trades", "journal", "lab", "ops", "about"]:
        page.locator(f'button[data-tab="{tab}"]').click()
        page.evaluate("window.scrollTo(0,0)")
        page.wait_for_timeout(600)
        frames.append(Image.open(io.BytesIO(page.screenshot())).convert("RGB"))
        if tab in ["desk", "journal", "about"]:
            page.evaluate("window.scrollTo(0,550)")
            page.wait_for_timeout(300)
            frames.append(Image.open(io.BytesIO(page.screenshot())).convert("RGB"))
    browser.close()
    if errors:
        raise RuntimeError(errors)
output = Path(__file__).resolve().parents[1] / "docs/dashboard.gif"
frames[0].save(output, save_all=True, append_images=frames[1:], duration=2400, loop=0, optimize=True)
print(f"Recorded {len(frames)} frames: {output}")
