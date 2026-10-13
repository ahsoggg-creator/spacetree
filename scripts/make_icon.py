"""Draws the SpaceTree icon (a tiny treemap) into crates/app/assets/.

Run: python scripts/make_icon.py
Outputs icon.ico (16..256 px, exe resource), icon.png (256 px) and icon64.rgba (raw window icon).
"""

from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "crates" / "app" / "assets"
S = 1024  # drawn large, downscaled with antialiasing


def draw() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((32, 32, S - 32, S - 32), radius=200, fill=(30, 34, 46, 255))
    pad, gap = 150, 34
    x0, y0, x1, y1 = pad, pad, S - pad, S - pad
    split = x0 + (x1 - x0) * 0.56
    tiles = [
        ((x0, y0, split - gap / 2, y1), (76, 141, 255)),  # big blue column
        ((split + gap / 2, y0, x1, y0 + (y1 - y0) * 0.48 - gap / 2), (240, 160, 60)),  # orange
        ((split + gap / 2, y0 + (y1 - y0) * 0.48 + gap / 2, x1, y1 - (y1 - y0) * 0.22 - gap / 2), (80, 200, 120)),
        ((split + gap / 2, y1 - (y1 - y0) * 0.22 + gap / 2, split + (x1 - split) * 0.5, y1), (190, 120, 230)),
        ((split + (x1 - split) * 0.5 + gap, y1 - (y1 - y0) * 0.22 + gap / 2, x1, y1), (230, 90, 110)),
    ]
    for (a, b, c, e), col in tiles:
        d.rounded_rectangle((a, b, c, e), radius=46, fill=col + (255,))
    return img


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    big = draw()
    big.resize((256, 256), Image.LANCZOS).save(OUT / "icon.png")
    big.resize((256, 256), Image.LANCZOS).save(
        OUT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    )
    # raw 64x64 RGBA for the window icon (no PNG decoder needed at runtime)
    (OUT / "icon64.rgba").write_bytes(big.resize((64, 64), Image.LANCZOS).tobytes())
    print("written to", OUT)


if __name__ == "__main__":
    main()
