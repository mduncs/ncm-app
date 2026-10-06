#!/usr/bin/env python3
"""Check fragmented real-client protocol decoding before trusting screen evidence."""
import codecs
from collections import Counter
import os
import time
import unittest

import pyte

from harness import Terminal, VTScreen, VTStream, WIDTH, HEIGHT


class ProtocolChecks(unittest.TestCase):
    def terminal(self):
        t = Terminal.__new__(Terminal)
        t.screen = VTScreen(WIDTH, HEIGHT)
        t.stream = VTStream(t.screen)
        t.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        t.pending = b""
        t.text_at = time.monotonic()
        t.stats = Counter()
        t.images = {}
        t.upload = None
        t.clipboard = b""
        t.events = []
        reader, t.master = os.pipe()
        self.addCleanup(os.close, reader)
        self.addCleanup(os.close, t.master)
        return t, reader

    def test_fragmented_passthrough_and_vt(self):
        kitty = b"\x1b_Ga=T,i=7,f=32,m=0;AAAA\x1b\\"
        wrapped = b"\x1bPtmux;" + kitty.replace(b"\x1b", b"\x1b\x1b") + b"\x1b\\"
        wire = b"\x1b[2J\x1b[3;5H" + "héllo".encode() + wrapped + b"\x1b[4;2H\x1b[48;2;240;243;248mSELECT"
        for stride in (1, 2, 7, len(wire)):
            with self.subTest(stride=stride):
                t, _ = self.terminal()
                for pos in range(0, len(wire), stride):
                    t.protocol(wire[pos:pos + stride])
                self.assertEqual(t.screen.display[2][4:9], "héllo")
                self.assertEqual(t.screen.display[3][1:7], "SELECT")
                self.assertEqual(t.screen.buffer[3][1].bg, "f0f3f8")
                self.assertEqual(t.stats["image_uploads"], 1)
                self.assertEqual(t.stats["graphics_bytes"], len(kitty))
                t.protocol(wrapped)
                self.assertEqual(t.stats["identical_image_resends"], 1)
                self.assertEqual(t.pending, b"")

    def test_rep_placeholders_clear_preview_without_stale_text(self):
        t, _ = self.terminal()
        anchor = "\U0010eeee\u030d\u0305"
        wire = ("\x1b[3;1HScreen Talk Reports\x1b[3;1H" + anchor
                + "\U0010eeee\x1b[20bEND").encode()
        for byte in wire:
            t.protocol(bytes([byte]))
        self.assertEqual(t.screen.buffer[2][0].data, anchor)
        self.assertEqual(t.screen.buffer[2][1].data, "\U0010eeee")
        self.assertEqual(t.screen.buffer[2][21].data, "\U0010eeee")
        self.assertEqual(t.screen.buffer[2][22].data, "E")
        self.assertNotIn("Reports", t.screen.display[2])

    def test_graphics_do_not_merge_separate_text_bursts(self):
        t, _ = self.terminal()
        t.protocol(b"\x1b[2J")
        t.text_at = time.monotonic() - .1
        t.protocol(b"\x1b_Ga=T,i=7,m=0;AAAA" + bytes([27, 92]))
        t.protocol(b"\x1b[1;1Hsmall update")
        self.assertEqual(t.stats["full_screen_redraws"], 1)
        t.flush_rows()
        self.assertEqual(t.stats["full_screen_redraws"], 1)
        self.assertEqual(t.stats["text_bursts"], 2)

    def test_query_response_and_clipboard(self):
        t, reader = self.terminal()
        for byte in b"\x1b_Ga=q,i=255;AAAA\x1b\\":
            t.protocol(bytes([byte]))
        reply = os.read(reader, 1024)
        self.assertIn(b"i=255;OK", reply)
        t.protocol(b"\x1b]52;c;Q0xJUEJPQVJEX1BST0JF\x07")
        self.assertEqual(t.clipboard, b"CLIPBOARD_PROBE")
        self.assertEqual(t.stats["queries"], 1)
        self.assertFalse(any("AAAA" in line for line in t.screen.display))

    def test_alternate_screen_return(self):
        t, _ = self.terminal()
        t.protocol(b"original\x1b[?1049hpreview\x1b[?1049l")
        self.assertTrue(t.screen.display[0].startswith("original"))
        self.assertFalse(t.screen.in_alternate)
        self.assertEqual(t.stats["alternate_exits"], 1)


if __name__ == "__main__":
    unittest.main()
