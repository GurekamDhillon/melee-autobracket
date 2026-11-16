"""Rename a font's name-table entries (family/full/PostScript/unique) so a modified
version does not carry the upstream name. usage: rename.py in.ttf out.ttf "Family" "Style" """
import sys
from fontTools.ttLib import TTFont
src, dst, family, style = sys.argv[1:5]
f = TTFont(src)
ps = family.replace(" ", "") + "-" + style.replace(" ", "")
new = {1: family, 2: style, 3: ps, 4: family + " " + style, 6: ps, 16: family, 17: style}
name = f["name"]
for rec in list(name.names):
    if rec.nameID in new:
        rec.string = new[rec.nameID]
for nid in (21, 22, 25):
    name.removeNames(nameID=nid)
f.save(dst)
