"""Point Inter's digits at its tabular glyphs, and nothing else.

egui cannot select OpenType features. Freezing the whole `tnum` feature
also makes the space, hyphen, brackets and other punctuation tabular,
which spaces out words such as "label-based". This maps only 0-9 to their
`.tf` glyphs, so counts like "12/34" keep their width as they change.

    pip install fonttools
    python3 freeze_digits.py InterVariable.ttf ../InterVariable-SignoffDigits.ttf
"""

import sys

from fontTools.ttLib import TTFont


def main(source: str, target: str) -> None:
    font = TTFont(source)
    glyphs = set(font.getGlyphOrder())
    for table in font["cmap"].tables:
        if not table.isUnicode():
            continue
        for code in range(ord("0"), ord("9") + 1):
            name = table.cmap.get(code)
            if name and f"{name}.tf" in glyphs:
                table.cmap[code] = f"{name}.tf"
    font.save(target)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
