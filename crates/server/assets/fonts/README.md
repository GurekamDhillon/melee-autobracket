# Fonts for the bracket page

Two typefaces, self-hosted so the page makes no external requests. Both are licensed under the
SIL Open Font License 1.1; each licence text is beside its file. They are served at
`/assets/fonts/<file>` (see `crates/server/src/routes/view.rs`).

| File | What it is | Licence | Bytes | SHA-256 |
|---|---|---|---|---|
| `bricolage-grotesque-800.woff2` | Bricolage Grotesque, static instance opsz 24, wdth 100, wght 800, subset | `bricolage-grotesque-OFL.txt` | 21340 | `9bd7db6abfc4ebca14291bea459d67d4547a311cc852f6d6ba565cd84fae1cea` |
| `autobracket-text-700.woff2` | Source Sans 3, static instance wght 700, subset, **renamed** "Autobracket Text" | `autobracket-text-OFL.txt` | 13448 | `6e3c53c9da7a58fb670b11ad2bea64dbadeee42f288d4a5e89767b0c0e056bf7` |

## Why one is renamed

Source Sans 3 is published "with Reserved Font Name 'Source'". Under the OFL a modified version
(an instance or a subset is one) may not carry a reserved name, so the file's own name records
say "Autobracket Text" and its file name does not use the reserved word. The copyright and
licence records are kept. Bricolage Grotesque declares no reserved name and keeps its own.

## Sources

Both originals come from Google Fonts' repository, `github.com/google/fonts`, at commit
`cc39c4c2c304bc368c9b7c311ea2bcc6013299fe` (main, 2026-11-14), downloaded 2026-11-15.

| Original (path in that repository) | Version | Upstream | Bytes | SHA-256 |
|---|---|---|---|---|
| `ofl/bricolagegrotesque/BricolageGrotesque[opsz,wdth,wght].ttf` | 1.001 | `github.com/ateliertriay/bricolage` commit `84745e5b96261ae5f8c6c856e262fe78d1d6efdd` | 408496 | `413e7357809ddd12fd80a96a8a396de0e401638d4acd3cb3e37532f0472ac682` |
| `ofl/bricolagegrotesque/OFL.txt` | | | 4403 | `4b5a7d8f37f5602621c8a8d7358a6a2e71317e6c231c661e15aef0275d3e07ba` |
| `ofl/sourcesans3/SourceSans3[wght].ttf` | 3.052 | `github.com/adobe-fonts/source-sans` commit `272b22b02e097e8eff1372111f88b5ab6063499f` | 646340 | `042fe2cc0b933e328410d7acbd0aa6a1873dca5aef81875f4bc214b08825c7b9` |
| `ofl/sourcesans3/OFL.txt` | | | 4579 | `09746787287a289323b0ec3cff4d1a4a801331b82b7207c1e186f5d26619a392` |

The licence files here are those two `OFL.txt` files, byte for byte, under new names.

## How the files were produced

With Python 3 and `fonttools==4.68.0`, `brotli==1.2.1` in a virtualenv, the originals saved as
`Bricolage.ttf` and `SourceSans3.ttf` in one directory:

    bash tools/build-fonts.sh <venv>/bin/python <dir with the originals> <output dir> tools/rename.py

`tools/build-fonts.sh` runs, in order:

    python -m fontTools varLib.instancer Bricolage.ttf opsz=24 wdth=100 wght=800 -o bg-800.ttf
    python -m fontTools varLib.instancer SourceSans3.ttf wght=700 -o ss-700.ttf
    python tools/rename.py ss-700.ttf ss-700-renamed.ttf "Autobracket Text" "Bold"
    python -m fontTools subset <in>.ttf \
      --unicodes=U+0020-007E,U+00A0-017F,U+2013,U+2014,U+2018,U+2019,U+201C,U+201D,U+2022,U+2026,U+2212 \
      --layout-features=kern,mark,mkmk,ccmp,locl,tnum,lnum --no-hinting --desubroutinize \
      --notdef-outline --name-IDs=0,1,2,3,4,5,6,13,14 --flavor=woff2 --output-file=<out>.woff2

The subset covers Basic Latin, Latin-1 Supplement and Latin Extended-A plus a few dashes, quotes,
the bullet, the ellipsis and the minus sign. Text in other scripts falls back to system fonts.
Bricolage Grotesque's optical size is fixed at 24, the middle of the sizes the page uses.

`fonttools` stamps the build time into each file, so a rebuild matches these hashes in glyphs
and tables but not byte for byte.
