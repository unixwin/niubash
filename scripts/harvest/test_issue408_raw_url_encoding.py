"""niubash#408 regression: raw content URLs must percent-encode path segments.

eco-harvest-ci run 39078222286 left 78/400 FETCH-FAILED rows, all of them
systematic: asset paths containing literal spaces (74) or '&' (4) were put
on the wire unencoded and raw.githubusercontent rejected the request,
while the percent-encoded form of the same URL fetches 200. The fix
(niubash PR #410) encodes each path segment separately so '/' separators
survive. These fixtures pin that contract with no network access.
"""

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "eco_harvest", Path(__file__).with_name("eco-harvest.py"))
HARVEST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARVEST)


class RawUrlEncoding408(unittest.TestCase):
    def test_space_in_filename_is_percent_encoded(self):
        url = HARVEST.raw_url("owner/repo", "abc123", "themes/my theme.zsh")
        self.assertEqual(
            url,
            "https://raw.githubusercontent.com/owner/repo/abc123/"
            "themes/my%20theme.zsh")

    def test_ampersand_in_filename_is_percent_encoded(self):
        url = HARVEST.raw_url("owner/repo", "abc123", "a&b/c d.sh")
        self.assertIn("a%26b/c%20d.sh", url)
        self.assertNotIn("a&b", url)

    def test_separators_and_plain_segments_untouched(self):
        url = HARVEST.raw_url("owner/repo", None, "plain/path/file.sh")
        self.assertTrue(url.endswith("/HEAD/plain/path/file.sh"))

    def test_non_ascii_segment_is_utf8_percent_encoded(self):
        url = HARVEST.raw_url("owner/repo", "abc123", "café.sh")
        self.assertIn("caf%C3%A9.sh", url)


if __name__ == "__main__":
    unittest.main()
