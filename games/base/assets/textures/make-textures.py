"""Draw the base game's terrain atlas.

Every pixel is generated here from a fixed seed, so the atlas is reproducible
and the repository carries no third-party art. The atlas is 64x16: three 16x16
tiles followed by the white fallback pixel used by materials without a tile.
"""

from pathlib import Path
from random import Random

from PIL import Image

TILE = 16
ATLAS = (64, 16)
HERE = Path(__file__).resolve().parent


def stone(rng: Random) -> Image.Image:
    image = Image.new("RGBA", (TILE, TILE))
    for y in range(TILE):
        for x in range(TILE):
            value = 150 + rng.randint(-22, 18)
            image.putpixel((x, y), (value, value - 2, value - 6, 255))
    for _ in range(10):
        x = rng.randrange(TILE)
        y = rng.randrange(TILE)
        image.putpixel((x, y), (104, 100, 94, 255))
    return image


def glass() -> Image.Image:
    image = Image.new("RGBA", (TILE, TILE), (168, 205, 218, 150))
    for i in range(TILE):
        image.putpixel((i, 0), (208, 236, 244, 210))
        image.putpixel((i, TILE - 1), (208, 236, 244, 210))
        image.putpixel((0, i), (208, 236, 244, 210))
        image.putpixel((TILE - 1, i), (208, 236, 244, 210))
    for i in range(3, 11):
        image.putpixel((i, 13 - i), (236, 250, 255, 230))
    return image


def plant() -> Image.Image:
    image = Image.new("RGBA", (TILE, TILE), (0, 0, 0, 0))
    blades = [(4, 15, 3), (7, 15, 5), (10, 15, 2), (12, 15, 4)]
    for base_x, base_y, height in blades:
        for step in range(height):
            y = base_y - step
            x = base_x + (1 if step > height // 2 else 0)
            shade = 120 + step * 12
            image.putpixel((x, y), (60, shade, 48, 255))
    image.putpixel((7, 15), (74, 140, 58, 255))
    return image


def main() -> None:
    rng = Random(20261025)
    atlas = Image.new("RGBA", ATLAS, (0, 0, 0, 0))
    atlas.paste(stone(rng), (0, 0))
    atlas.paste(glass(), (16, 0))
    atlas.paste(plant(), (32, 0))
    atlas.putpixel((48, 0), (255, 255, 255, 255))
    atlas.save(HERE / "atlas.png")
    print(f"wrote {HERE / 'atlas.png'} ({ATLAS[0]}x{ATLAS[1]})")


if __name__ == "__main__":
    main()
