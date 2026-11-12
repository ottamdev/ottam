"""Normalize a checkout label from stdin and cap it at 12 visible characters.

Count user-perceived characters so decomposed accents and joined emoji stay
intact. The stdlib has no grapheme segmentation, so this approximates UAX #29:
combining marks, joiners, variation selectors, emoji modifiers, tag characters,
and regional-indicator pairs extend the character before them. Hangul jamo and
Indic conjuncts are not joined. Run by scripts/app-display-name.sh.
"""
from __future__ import annotations

import sys
import unicodedata

LIMIT = 12
ZWJ = "‍"
ZWNJ = "‌"


def is_regional_indicator(character: str) -> bool:
    return 0x1F1E6 <= ord(character) <= 0x1F1FF


def is_pictographic(character: str) -> bool:
    code = ord(character)
    return code >= 0x1F000 or 0x2600 <= code <= 0x27BF or unicodedata.category(character) == "So"


def extends(character: str) -> bool:
    code = ord(character)
    return (
        unicodedata.category(character) in ("Mn", "Me", "Mc")
        or character in (ZWJ, ZWNJ)
        or 0xFE00 <= code <= 0xFE0F
        or 0xE0100 <= code <= 0xE01EF
        or 0x1F3FB <= code <= 0x1F3FF
        or 0xE0020 <= code <= 0xE007F
    )


def clusters(text: str) -> list[str]:
    result: list[str] = []
    for character in text:
        if result:
            previous = result[-1]
            joins = (
                extends(character)
                or (previous.endswith(ZWJ) and is_pictographic(character))
                or (len(previous) == 1 and is_regional_indicator(previous) and is_regional_indicator(character))
            )
            if joins:
                result[-1] = previous + character
                continue
        result.append(character)
    return result


def normalize(text: str) -> str:
    # Whitespace and control characters collapse to one space; format
    # characters such as the zero-width joiner stay.
    pieces: list[str] = []
    for character in text:
        if character.isspace() or unicodedata.category(character) == "Cc":
            if not pieces or pieces[-1] != " ":
                pieces.append(" ")
        else:
            pieces.append(character)
    return "".join(pieces).strip(" ") or "worktree"


def main() -> int:
    characters = clusters(normalize(sys.stdin.buffer.read().decode("utf-8", errors="replace")))
    label = "".join(characters[: LIMIT - 1]) + "…" if len(characters) > LIMIT else "".join(characters)
    sys.stdout.buffer.write(label.encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
