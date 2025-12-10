import builtins
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from urllib.error import HTTPError

from bing_wallpaper.cli import (
    Settings,
    WallpaperError,
    build_archive_url,
    download_image,
    normalize_auto_update_name,
    sanitize_filename,
)


def make_settings(tmpdir: Path, *, filename: str | None = None, force: bool = False) -> Settings:
    return Settings(
        proto="https",
        country=None,
        day=0,
        picture_dir=tmpdir,
        auto_update_name="default",
        monitor=0,
        force=force,
        quiet=True,
        experimental=False,
        filename=filename,
    )


class DummyResponse:
    def __init__(self, body: bytes, status: int = 200):
        self.body = body
        self.status = status

    def read(self) -> bytes:
        return self.body

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        return False


class CliTests(unittest.TestCase):
    def test_normalize_auto_update_name(self):
        self.assertEqual(normalize_auto_update_name("  "), "default")
        self.assertEqual(normalize_auto_update_name("foo bar"), "foo-bar")
        self.assertEqual(normalize_auto_update_name("Name_1"), "Name_1")

    def test_sanitize_filename(self):
        self.assertEqual(sanitize_filename(""), "wallpaper.jpg")
        self.assertEqual(sanitize_filename("custom"), "custom.jpg")
        self.assertEqual(sanitize_filename("dir/../name.png"), "name.png")

    def test_build_archive_url_includes_country(self):
        url = build_archive_url(1, "en-US")
        self.assertIn("idx=1", url)
        self.assertIn("mkt=en-US", url)

    def test_download_image_skips_existing_without_force(self):
        with tempfile.TemporaryDirectory() as td:
            tmpdir = Path(td)
            settings = make_settings(tmpdir)
            target = tmpdir / "default-urlbase_1920x1080.jpg"
            target.write_bytes(b"existing")
            metadata = b"<xml />"

            with mock.patch("bing_wallpaper.cli.urllib.request.urlopen") as urlopen:
                path, skipped = download_image(
                    url_base="urlbase",
                    resolution="1920x1080",
                    settings=settings,
                    metadata_body=metadata,
                )

            self.assertTrue(skipped)
            self.assertEqual(path, target)
            urlopen.assert_not_called()

    def test_download_image_success_replaces_old_after_complete(self):
        with tempfile.TemporaryDirectory() as td:
            tmpdir = Path(td)
            settings = make_settings(tmpdir, filename=None, force=False)

            old_wallpaper = tmpdir / "default-old.jpg"
            old_wallpaper.write_bytes(b"old")
            metadata = b"<info>meta</info>"

            dummy_response = DummyResponse(b"image-bytes", status=200)
            with mock.patch("bing_wallpaper.cli.urllib.request.urlopen", return_value=dummy_response):
                path, skipped = download_image(
                    url_base="urlbase",
                    resolution="1920x1080",
                    settings=settings,
                    metadata_body=metadata,
                )

            self.assertFalse(skipped)
            self.assertTrue(path.exists())
            self.assertEqual(path.read_bytes(), b"image-bytes")
            self.assertFalse(old_wallpaper.exists(), "Old wallpapers should be removed after success")
            self.assertEqual((tmpdir / "info.xml").read_bytes(), metadata)

    def test_download_image_http_error_cleans_temp(self):
        with tempfile.TemporaryDirectory() as td:
            tmpdir = Path(td)
            settings = make_settings(tmpdir)
            metadata = b"meta"
            target = tmpdir / "default-urlbase_1920x1080.jpg"
            temp = target.with_suffix(f"{target.suffix}.tmp")

            def fake_urlopen(*args, **kwargs):
                # Provide a BytesIO fp so HTTPError does not leak resources
                from io import BytesIO

                raise HTTPError("http://example.com", 404, "not found", hdrs=None, fp=BytesIO())

            with mock.patch("bing_wallpaper.cli.urllib.request.urlopen", side_effect=fake_urlopen):
                with self.assertRaises(WallpaperError):
                    download_image(
                        url_base="urlbase",
                        resolution="1920x1080",
                        settings=settings,
                        metadata_body=metadata,
                    )

            self.assertFalse(target.exists())
            self.assertFalse(temp.exists())


if __name__ == "__main__":
    unittest.main()
