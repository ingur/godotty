# Bundled fonts

Embedded in the extension binary, licensed under the SIL Open Font License 1.1
([OFL.txt](OFL.txt)):

- JetBrains Mono Nerd Font — Copyright 2020 The JetBrains Mono Project Authors
- Noto Emoji — Copyright 2013 Google LLC (monochrome media/clock symbols)
- JuliaMono — Copyright 2020-2023 cormullion (monochrome math/technical/braille)
- Noto Color Emoji — Copyright 2013 Google LLC

Glyph resolution per codepoint: the primary font first, then a fallback
chain ordered by the cell's presentation. Text cells try Noto Emoji and
JuliaMono for monochrome symbols, then Noto Color Emoji, then installed
system fonts for other scripts. Emoji cells try Noto Color Emoji first,
then the monochrome fonts, then system fonts. Text-presentation cells
prefer monochrome; emoji cells prefer color.
