#!/usr/bin/env python3
"""Generate synthetic fixtures (no PTY): origin mode, scroll regions,
DECSLRM, the saved cursor, Kitty graphics and sixel.

Each fixture is fixtures/<name>.bin plus fixtures/<name>.json, the same
format as S1's recordings, at 100x30. Probes (sequences the harness sends to
both the source and the restored terminal afterwards, to compare state that
isn't directly readable) live in the harness, not here.
"""
import base64, json, os, zlib, struct

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")
E = "\x1b"
CSI = E + "["


def filler(n, width=98):
    return "".join(f"{CSI}0mline {i:03d} " + ("abcdefghij" * 10)[: width - 9] + "\r\n" for i in range(n))


FIXTURES = {}

# (b) origin mode + top/bottom scroll region, left on, with text scrolled
# inside the region and protected cells (DECSCA) under it.
FIXTURES["decom"] = (
    filler(40)
    + f"{CSI}2J{CSI}H"
    + "outside region: top row\r\n"
    + f"{CSI}30;1Houtside region: bottom row"
    + f"{CSI}5;20r"  # DECSTBM rows 5..20
    + f"{CSI}?6h"  # DECOM: CUP is relative to the region
    + f"{CSI}1;1H{CSI}1;33mregion row 1 (origin){CSI}0m"
    + "".join(f"{CSI}{i};3Hr{i:02d}" for i in range(2, 17))
    + f"{CSI}16;1H"
    + "\n" * 5  # scroll the region up 5 times
    + f'{CSI}1"q' + "PROTECTED" + f'{CSI}0"q' + " plain"  # DECSCA on/off
    + f"{CSI}3;10H{CSI}1\"q{CSI}7mprot+inverse{CSI}0m"  # leave DECSCA on
    + f"{CSI}8;40H"
)

# (b) left/right margins (DECLRMM + DECSLRM) with a scroll region, no origin
# mode: wrapped text inside the margins, IL/DL, SU, ICH/DCH.
FIXTURES["slrm"] = (
    filler(35)
    + f"{CSI}?69h"  # DECLRMM
    + f"{CSI}4;18r"  # DECSTBM
    + f"{CSI}20;70s"  # DECSLRM cols 20..70
    + f"{CSI}6;20H{CSI}44m"
    + ("wrapped inside the margins " * 6)
    + f"{CSI}0m{CSI}10;25H{CSI}2L"  # IL inside margins
    + f"{CSI}12;25H{CSI}1M"  # DL
    + f"{CSI}2S"  # SU inside the region
    + f"{CSI}15;30H{CSI}4@INS"  # ICH
    + f"{CSI}16;30H{CSI}3P"  # DCH
    + f"{CSI}17;40H"
)

# (b) both: DECLRMM + DECSLRM + DECSTBM + DECOM, cursor homed into the box.
FIXTURES["slrm_decom"] = (
    filler(30)
    + f"{CSI}?69h{CSI}5;15r{CSI}30;60s{CSI}?6h"
    + f"{CSI}1;1H{CSI}32mbox home{CSI}0m"
    + f"{CSI}11;1H" + ("box text that wraps at the right margin " * 3)
    + "\n\n\n"  # scroll the box
    + f"{CSI}4;7H"
)

# (c) DECSC in the primary screen with attributes, charset and origin mode;
# then the cursor moves on with different attributes.
FIXTURES["decsc_primary"] = (
    filler(10)
    + f"{CSI}7;12H{CSI}1;4;38;5;208;48;2;10;20;30m"
    + f"{E}(0"  # G0 = DEC special graphics
    + f"{E}7"  # DECSC
    + f"{E}(B{CSI}0m{CSI}22;50Hafter save"
)

# (c) DECSC with pending wrap: save while the cursor sits past the last column.
FIXTURES["decsc_wrap"] = filler(5) + f"{CSI}12;1H{CSI}35m" + "w" * 100 + f"{E}7{CSI}0m{CSI}20;5Hmoved"

# (c) alt screen through 1049 (which itself saves the primary cursor), with a
# separate DECSC inside the alt screen.
FIXTURES["decsc_1049"] = (
    filler(12)
    + f"{CSI}3;3H{CSI}31m{E}7"  # primary DECSC at 3;3 red (1049h overwrites it)
    + f"{CSI}14;9H{CSI}1;32m"  # where 1049h saves: 14;9 bold green
    + f"{CSI}?1049h{CSI}H{CSI}2J"
    + "alt screen\r\n"
    + f"{CSI}6;30H{CSI}3;34m{E}7"  # alt DECSC 6;30 italic blue
    + f"{CSI}0m{CSI}25;1Halt after save"
)

# (c) alt screen through 47 (no save): each screen keeps its own DECSC.
FIXTURES["decsc_47"] = (
    filler(12)
    + f"{CSI}3;3H{CSI}31m{E}7"  # primary DECSC 3;3 red
    + f"{CSI}0m{CSI}13;1H"
    + f"{CSI}?47h{CSI}H{CSI}2J"
    + "alt via 47\r\n"
    + f"{CSI}6;30H{CSI}4;35m{E}7"  # alt DECSC 6;30 underline magenta
    + f"{CSI}0m{CSI}25;1Halt after save"
)


def kitty(params, payload=b""):
    data = base64.b64encode(payload).decode()
    chunks = [data[i : i + 4096] for i in range(0, len(data), 4096)] or [""]
    out = ""
    for i, c in enumerate(chunks):
        more = 1 if i < len(chunks) - 1 else 0
        p = (params + "," if i == 0 else "") + f"m={more}"
        out += f"{E}_G{p};{c}{E}\\"
    return out


def rgba(w, h):
    return bytes(
        v for y in range(h) for x in range(w) for v in ((x * 255) // w, (y * 255) // h, 128, 255)
    )


def diacritic(n):
    # First entries of kitty's rowcolumn-diacritics.txt.
    table = [0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F]
    return chr(table[n])


# (d) Kitty graphics: a direct placement and a virtual (Unicode placeholder)
# placement, with text around them.
FIXTURES["kitty"] = (
    "before image\r\n"
    + kitty("a=T,f=32,s=16,v=16,i=7,c=4,r=2,q=2", rgba(16, 16))
    + "\r\n\r\n\r\nafter image\r\n"
    + kitty("a=t,f=32,s=8,v=8,i=9,q=2", rgba(8, 8))
    + kitty("a=p,U=1,i=9,p=1,c=2,r=1,q=2")
    + f"{CSI}38;5;9m"  # placeholder fg encodes image id 9
    + "".join("\U0010EEEE" + diacritic(0) + diacritic(c) for c in range(2))
    + f"{CSI}0m <- virtual placement\r\n"
)

# (d) Sixel: a small DCS q image between two lines of text.
sixel = f"{E}Pq" + '"1;1;12;12' + "#0;2;100;0;0#0" + "~" * 12 + "-" + "#0" + "~" * 12 + f"{E}\\"
FIXTURES["sixel"] = "before sixel\r\n" + sixel + "after sixel\r\n"


def main():
    os.makedirs(OUT, exist_ok=True)
    for name, s in FIXTURES.items():
        b = s.encode()
        with open(os.path.join(OUT, name + ".bin"), "wb") as f:
            f.write(b)
        with open(os.path.join(OUT, name + ".json"), "w") as f:
            json.dump(dict(cols=100, rows=30, resizes=[]), f)
        print(f"{name}: {len(b)} bytes")


if __name__ == "__main__":
    main()
