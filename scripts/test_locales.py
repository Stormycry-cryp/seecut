"""Regression coverage for production strings following external test modules."""
import pathlib
import tempfile
import unittest
from unittest.mock import patch

import locales


class LocaleInventoryTests(unittest.TestCase):
    def test_external_test_declarations_do_not_hide_production_strings(self):
        with tempfile.TemporaryDirectory() as folder:
            crate = pathlib.Path(folder)
            (crate / "src").mkdir()
            (crate / "src" / "canvas.rs").write_text(
                '#[cfg(test)]\nmod c02_tests;\n'
                'fn save() { tf("Save project"); }\n'
                '#[cfg(test)]\nmod another_tests;\n'
                'fn export() { tf("Export as PNG"); }\n'
                'const ADJUSTMENT_KINDS: &[(i32, &str)] = &[(10, "Color Lookup")];\n'
                '#[cfg(test)]\nmod tests { fn fake() { tf("Fake test key"); } }\n',
                encoding="utf-8",
            )
            with patch.object(locales, "CRATE", crate), patch.object(locales, "PACKAGES", crate / "packages"):
                self.assertEqual(locales.keys(), {"Save project", "Export as PNG", "Color Lookup", "Other"})


if __name__ == "__main__":
    unittest.main()
