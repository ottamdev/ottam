#!/usr/bin/env python3
"""Generate the Ottam brand/icon asset set from approved SVG marks.

Sources in assets/brand are named <role>-<theme>.svg, where the theme is the
appearance the asset is meant for: icon-light.svg is the icon for light mode,
lockup-dark.svg is the lockup for dark backgrounds. mark.svg is the bare
thread and works on both.

Brand PNGs and favicons: assets/brand/icon-dark.svg (png/ottam-icon-*).
The other sources get PNGs too: png/ottam-icon-light-*, png/ottam-mark-*, and
png/ottam-lockup-{light,dark}-*. Square sources are named by edge length; the
lockups are taller than wide and are named by height. avatar-{dark,light}.svg
are the icons without rounded corners, for profile pictures that sites crop
themselves (png/ottam-avatar-*).
macOS app icon: assets/brand/Ottam.icon, an Icon Composer document compiled to
Assets.car (needs Xcode 26+). macOS draws that one, with its dark, tinted and
clear appearances. assets/brand/icon-light.svg feeds the .icns fallback, which
holds one appearance and which macOS 26+ redraws flat.

Ottam.icon carries its left-to-right gradients as artwork, and the dark thread
as its own layer: Icon Composer draws gradient fills top to bottom whatever
their orientation, and flattens a gradient image swapped in per appearance.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    sys.exit("generate-brand-assets.py needs Pillow, the one script dependency "
             "outside the stdlib: python3 -m pip install --user Pillow")


ROOT = Path(__file__).resolve().parents[1]
BRAND_DIR = ROOT / "assets" / "brand"
BRAND_SOURCE_PATH = BRAND_DIR / "icon-dark.svg"
APP_ICON_SOURCE_PATH = BRAND_DIR / "icon-light.svg"
COMPOSER_ICON_PATH = BRAND_DIR / "Ottam.icon"
PNG_DIR = BRAND_DIR / "png"
FAVICON_DIR = BRAND_DIR / "favicon"
APP_RESOURCES_DIR = ROOT / "app" / "Resources" / "AppResources"
ICONSET_DIR = APP_RESOURCES_DIR / "Ottam.iconset"
BUILD_DIR = ROOT / ".build" / "brand-assets"

PNG_SIZES = [16, 32, 48, 64, 128, 180, 192, 256, 512, 1024, 2048, 4096]
LOCKUP_HEIGHTS = [256, 512, 1024, 2048]
AVATAR_SIZES = [512, 1024]

# (source in assets/brand, PNG name prefix, sizes) for the sources besides the
# brand icon, which also feeds the favicons and stays at png/ottam-icon-*.
EXTRA_PNG_SETS = [
    ("icon-light.svg", "ottam-icon-light", PNG_SIZES),
    ("mark.svg", "ottam-mark", PNG_SIZES),
    ("lockup-light.svg", "ottam-lockup-light", LOCKUP_HEIGHTS),
    ("lockup-dark.svg", "ottam-lockup-dark", LOCKUP_HEIGHTS),
    ("avatar-dark.svg", "ottam-avatar-dark", AVATAR_SIZES),
    ("avatar-light.svg", "ottam-avatar-light", AVATAR_SIZES),
]

ICONSET_SPECS = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]

FAVICON_SPECS = {
    "favicon-16x16.png": 16,
    "favicon-32x32.png": 32,
    "favicon-48x48.png": 48,
    "apple-touch-icon.png": 180,
    "android-chrome-192x192.png": 192,
    "android-chrome-512x512.png": 512,
}

WEBMANIFEST = """{
  "name": "Ottam",
  "short_name": "Ottam",
  "icons": [
    { "src": "/android-chrome-192x192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/android-chrome-512x512.png", "sizes": "512x512", "type": "image/png" }
  ],
  "theme_color": "#0A0B0D",
  "background_color": "#0A0B0D",
  "display": "standalone"
}
"""


def require(tool: str) -> None:
    if shutil.which(tool) is None:
        raise SystemExit(f"required tool not found on PATH: {tool}")


def render_svg_master(source: Path, output: Path, size: int = 4096) -> None:
    subprocess.run(
        [
            "rsvg-convert",
            "--format",
            "png",
            "--width",
            str(size),
            "--height",
            str(size),
            "--output",
            str(output),
            str(source),
        ],
        cwd=ROOT,
        check=True,
    )


def write_resized(master: Image.Image, output: Path, size: int) -> None:
    resized = master.resize((size, size), Image.Resampling.LANCZOS)
    resized.save(output)


def write_png_set(source: Path, prefix: str, heights: list[int]) -> None:
    """Render source once at the largest height, then downscale, keeping its aspect ratio."""
    largest = max(heights)
    master_path = PNG_DIR / f"{prefix}-{largest}.png"
    # Only --height is given, so rsvg-convert scales the width to match.
    subprocess.run(
        ["rsvg-convert", "--format", "png", "--height", str(largest),
         "--output", str(master_path), str(source)],
        cwd=ROOT,
        check=True,
    )
    master = Image.open(master_path).convert("RGBA")
    for height in heights:
        if height == largest:
            continue
        width = max(1, round(master.width * height / master.height))
        master.resize((width, height), Image.Resampling.LANCZOS).save(
            PNG_DIR / f"{prefix}-{height}.png"
        )


def write_ico() -> None:
    base = Image.open(PNG_DIR / "ottam-icon-512.png").convert("RGBA")
    base.save(
        FAVICON_DIR / "favicon.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48)],
    )


def compile_composer_icon() -> None:
    """Compile Ottam.icon to Assets.car; Info.plist names it in CFBundleIconName."""
    output_dir = BUILD_DIR / "composer-icon"
    shutil.rmtree(output_dir, ignore_errors=True)
    output_dir.mkdir(parents=True)
    result = subprocess.run(
        [
            "xcrun",
            "actool",
            str(COMPOSER_ICON_PATH),
            "--compile",
            str(output_dir),
            "--app-icon",
            COMPOSER_ICON_PATH.stem,
            "--platform",
            "macosx",
            "--minimum-deployment-target",
            "26.0",
            "--output-partial-info-plist",
            str(output_dir / "partial.plist"),
            "--errors",
            "--warnings",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    # actool reports on stdout as a plist that always lists what it compiled,
    # so it is only worth showing when it also carries errors or warnings.
    compiled = output_dir / "Assets.car"
    failed = result.returncode != 0 or not compiled.exists()
    if failed or "errors</key>" in result.stdout or "warnings</key>" in result.stdout:
        print(result.stdout + result.stderr, file=sys.stderr)
    if failed:
        raise SystemExit(f"actool could not compile {COMPOSER_ICON_PATH}")
    shutil.copyfile(compiled, APP_RESOURCES_DIR / "Assets.car")


def main() -> None:
    require("iconutil")
    require("rsvg-convert")
    require("xcrun")
    if not BRAND_SOURCE_PATH.exists():
        raise SystemExit(f"missing brand source image: {BRAND_SOURCE_PATH}")
    if not APP_ICON_SOURCE_PATH.exists():
        raise SystemExit(f"missing app icon source image: {APP_ICON_SOURCE_PATH}")
    if not COMPOSER_ICON_PATH.is_dir():
        raise SystemExit(f"missing Icon Composer document: {COMPOSER_ICON_PATH}")

    for directory in (PNG_DIR, FAVICON_DIR, APP_RESOURCES_DIR, ICONSET_DIR, BUILD_DIR):
        directory.mkdir(parents=True, exist_ok=True)

    brand_master_path = PNG_DIR / "ottam-icon-4096.png"
    render_svg_master(BRAND_SOURCE_PATH, brand_master_path, 4096)
    brand_master = Image.open(brand_master_path).convert("RGBA")

    for size in PNG_SIZES:
        write_resized(brand_master, PNG_DIR / f"ottam-icon-{size}.png", size)

    for filename, size in FAVICON_SPECS.items():
        shutil.copyfile(PNG_DIR / f"ottam-icon-{size}.png", FAVICON_DIR / filename)

    for source_name, prefix, sizes in EXTRA_PNG_SETS:
        source = BRAND_DIR / source_name
        if not source.exists():
            raise SystemExit(f"missing brand source image: {source}")
        write_png_set(source, prefix, sizes)

    app_icon_master_path = BUILD_DIR / "ottam-macos-icon-4096.png"
    render_svg_master(APP_ICON_SOURCE_PATH, app_icon_master_path, 4096)
    app_icon_master = Image.open(app_icon_master_path).convert("RGBA")

    for filename, size in ICONSET_SPECS:
        write_resized(app_icon_master, ICONSET_DIR / filename, size)

    write_ico()
    (FAVICON_DIR / "site.webmanifest").write_text(WEBMANIFEST, encoding="utf-8")

    icns_path = APP_RESOURCES_DIR / "Ottam.icns"
    icns_path.unlink(missing_ok=True)
    subprocess.run(
        ["iconutil", "--convert", "icns", "--output", str(icns_path), str(ICONSET_DIR)],
        cwd=ROOT,
        check=True,
    )
    compile_composer_icon()
    print("brand assets regenerated from", BRAND_SOURCE_PATH.relative_to(ROOT))
    print("macOS app icon regenerated from", COMPOSER_ICON_PATH.relative_to(ROOT))
    print(".icns fallback regenerated from", APP_ICON_SOURCE_PATH.relative_to(ROOT))


if __name__ == "__main__":
    main()
