"""A small deterministic VT100/xterm screen model for the completion matrix.

Only the subset the editor's painter emits is implemented: CUP/HVP/VPA/CHA,
cursor motion, ED/EL, SGR (attributes + 16/256 colors), save/restore, scroll,
insert/delete characters/lines, and UTF-8 display widths. It exists so the
matrix can assert *frames* (which cell is reverse-video, which candidates sit
on which grid row) instead of grepping raw escape bytes.
"""

from __future__ import annotations

import codecs
import unicodedata

BLANK = (" ", False, False, None, None)


def char_width(ch: str) -> int:
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Screen:
    def __init__(self, rows: int = 24, cols: int = 80):
        self.rows = rows
        self.cols = cols
        self.cells = [[BLANK for _ in range(cols)] for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.reverse = False
        self.bold = False
        self.fg = None
        self.bg = None
        self.saved = (0, 0)
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self._buf = ""
        self.cpr_requests = 0

    # ── attributes ──────────────────────────────────────────────────
    def _attr(self):
        return (self.reverse, self.bold, self.fg, self.bg)

    def _blank(self):
        return (" ", *self._attr())

    def _erase(self, row, start, end):
        for col in range(max(0, start), min(self.cols, end)):
            self.cells[row][col] = self._blank()

    def _scroll_up(self):
        del self.cells[self.scroll_top]
        self.cells.insert(self.scroll_bottom, [self._blank() for _ in range(self.cols)])

    def _newline(self):
        if self.row == self.scroll_bottom:
            self._scroll_up()
        elif self.row < self.rows - 1:
            self.row += 1

    def _put(self, ch):
        width = char_width(ch)
        if width == 0:
            return
        if self.col + width > self.cols:
            self.col = 0
            self._newline()
        self.cells[self.row][self.col] = (ch, *self._attr())
        if width == 2 and self.col + 1 < self.cols:
            self.cells[self.row][self.col + 1] = ("\0", *self._attr())
        self.col += width

    # ── public ──────────────────────────────────────────────────────
    def feed(self, data: bytes) -> int:
        """Feed bytes; returns the number of cursor-position requests seen."""
        text = self._decoder.decode(data)
        self._buf += text
        while self._buf:
            ch = self._buf[0]
            if ch == "\x1b":
                used = self._escape()
                if used == 0:
                    return self.cpr_requests  # incomplete sequence, wait
                self._buf = self._buf[used:]
                continue
            if ch == "\r":
                self.col = 0
            elif ch == "\n":
                self._newline()
            elif ch == "\b":
                self.col = max(0, self.col - 1)
            elif ch == "\t":
                self.col = min(self.cols - 1, (self.col // 8 + 1) * 8)
            elif ch == "\x07":
                pass
            elif ch >= " ":
                self._put(ch)
            self._buf = self._buf[1:]
        return self.cpr_requests

    def _escape(self) -> int:
        """Consume one escape sequence. Returns bytes used, 0 if incomplete."""
        buf = self._buf
        if len(buf) < 2:
            return 0
        nxt = buf[1]
        if nxt == "[":
            i = 2
            while i < len(buf) and not ("\x40" <= buf[i] <= "\x7e"):
                i += 1
            if i >= len(buf):
                return 0
            body, final = buf[2:i], buf[i]
            self._csi(body, final)
            return i + 1
        if nxt == "]":
            i = 2
            while i < len(buf):
                if buf[i] == "\x07":
                    return i + 1
                if buf[i] == "\x1b" and i + 1 < len(buf) and buf[i + 1] == "\\":
                    return i + 2
                i += 1
            return 0
        if nxt == "7":
            self.saved = (self.row, self.col)
            return 2
        if nxt == "8":
            self.row, self.col = self.saved
            return 2
        if nxt == "D":
            self._newline()
            return 2
        if nxt == "M":
            if self.row == self.scroll_top:
                del self.cells[self.scroll_bottom]
                self.cells.insert(self.scroll_top, [self._blank() for _ in range(self.cols)])
            elif self.row > 0:
                self.row -= 1
            return 2
        if nxt == "E":
            self.col = 0
            self._newline()
            return 2
        if nxt in "()*+-./":
            return 3 if len(buf) >= 3 else 0
        if nxt == "c":
            self.__init__(self.rows, self.cols)
            return 2
        return 2

    @staticmethod
    def _params(body: str):
        body = body.lstrip("?<>=")
        if not body:
            return []
        out = []
        for part in body.split(";"):
            try:
                out.append(int(part) if part else 0)
            except ValueError:
                out.append(0)
        return out

    def _sgr(self, params):
        if not params:
            params = [0]
        i = 0
        while i < len(params):
            p = params[i]
            if p == 0:
                self.reverse = self.bold = False
                self.fg = self.bg = None
            elif p == 1:
                self.bold = True
            elif p == 22:
                self.bold = False
            elif p in (7, 27):
                self.reverse = p == 7
            elif 30 <= p <= 37:
                self.fg = p - 30
            elif 90 <= p <= 97:
                self.fg = p - 90 + 8
            elif p == 39:
                self.fg = None
            elif 40 <= p <= 47:
                self.bg = p - 40
            elif 100 <= p <= 107:
                self.bg = p - 100 + 8
            elif p == 49:
                self.bg = None
            elif p in (38, 48):
                # 38;5;n or 38;2;r;g;b
                if i + 1 < len(params) and params[i + 1] == 5:
                    if i + 2 < len(params):
                        if p == 38:
                            self.fg = params[i + 2]
                        else:
                            self.bg = params[i + 2]
                        i += 2
                elif i + 1 < len(params) and params[i + 1] == 2:
                    i += 4
            i += 1

    def _csi(self, body: str, final: str):
        params = self._params(body)
        first = params[0] if params else 0
        if final in "Hf":
            self.row = min(self.rows - 1, max(0, (params[0] or 1) - 1 if params else 0))
            self.col = min(self.cols - 1, max(0, (params[1] or 1) - 1 if len(params) > 1 else 0))
        elif final == "A":
            self.row = max(0, self.row - (first or 1))
        elif final == "B":
            self.row = min(self.rows - 1, self.row + (first or 1))
        elif final == "C":
            self.col = min(self.cols - 1, self.col + (first or 1))
        elif final == "D":
            self.col = max(0, self.col - (first or 1))
        elif final == "E":
            self.row = min(self.rows - 1, self.row + (first or 1))
            self.col = 0
        elif final == "F":
            self.row = max(0, self.row - (first or 1))
            self.col = 0
        elif final == "G":
            self.col = min(self.cols - 1, max(0, (first or 1) - 1))
        elif final == "d":
            self.row = min(self.rows - 1, max(0, (first or 1) - 1))
        elif final == "J":
            if first == 0:
                self._erase(self.row, self.col, self.cols)
                for r in range(self.row + 1, self.rows):
                    self._erase(r, 0, self.cols)
            elif first == 1:
                for r in range(0, self.row):
                    self._erase(r, 0, self.cols)
                self._erase(self.row, 0, self.col + 1)
            else:
                for r in range(self.rows):
                    self._erase(r, 0, self.cols)
        elif final == "K":
            if first == 0:
                self._erase(self.row, self.col, self.cols)
            elif first == 1:
                self._erase(self.row, 0, self.col + 1)
            else:
                self._erase(self.row, 0, self.cols)
        elif final == "m":
            self._sgr(params)
        elif final == "s":
            self.saved = (self.row, self.col)
        elif final == "u":
            self.row, self.col = self.saved
        elif final == "P":
            count = first or 1
            row = self.cells[self.row]
            del row[self.col : self.col + count]
            row.extend([self._blank() for _ in range(count)])
        elif final == "@":
            count = first or 1
            row = self.cells[self.row]
            for _ in range(count):
                row.insert(self.col, self._blank())
            del row[self.cols :]
        elif final == "X":
            count = first or 1
            self._erase(self.row, self.col, self.col + count)
        elif final == "L":
            count = first or 1
            for _ in range(count):
                self.cells.insert(self.row, [self._blank() for _ in range(self.cols)])
                del self.cells[self.rows]
        elif final == "M":
            count = first or 1
            for _ in range(count):
                del self.cells[self.row]
                self.cells.append([self._blank() for _ in range(self.cols)])
        elif final == "r":
            self.scroll_top = max(0, (params[0] or 1) - 1) if params else 0
            self.scroll_bottom = min(self.rows - 1, (params[1] or self.rows) - 1) if len(params) > 1 else self.rows - 1
            self.row, self.col = 0, 0
        elif final == "n" and first == 6:
            self.cpr_requests += 1

    # ── queries ─────────────────────────────────────────────────────
    def row_text(self, row: int) -> str:
        return "".join(c[0] for c in self.cells[row]).replace("\0", "").rstrip()

    def text(self) -> list[str]:
        return [self.row_text(r) for r in range(self.rows)]

    def cell(self, row: int, col: int):
        if 0 <= row < self.rows and 0 <= col < self.cols:
            return self.cells[row][col]
        return BLANK

    def find(self, needle: str):
        """First (row, col) of needle in the visible screen, or None."""
        for r in range(self.rows):
            text = self.row_text(r)
            c = text.find(needle)
            if c >= 0:
                return r, c
        return None

    def reversed_runs(self, row: int):
        """[(start, end_exclusive, text)] for reverse-video cells in a row."""
        runs = []
        start = None
        for c in range(self.cols):
            rev = self.cells[row][c][1]
            if rev and start is None:
                start = c
            elif not rev and start is not None:
                runs.append((start, c, self.row_text(row)[start:c]))
                start = None
        if start is not None:
            runs.append((start, self.cols, self.row_text(row)[start:]))
        return runs

    def is_reversed(self, row: int, col: int) -> bool:
        cell = self.cell(row, col)
        return bool(cell[1] and cell[0] not in (" ", "\0"))
