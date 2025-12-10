from __future__ import annotations

import argparse
import datetime as dt
import sqlite3
import subprocess
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional, Sequence, Tuple

from urllib.error import HTTPError
from . import __version__

DEFAULT_RESOLUTIONS: list[str] = [
    "1920x1200",
    "1920x1080",
    "1024x768",
    "1280x720",
    "1366x768",
    "UHD",
]
DEFAULT_PICTURE_DIR = Path.home() / "Pictures" / "bing-wallpapers"
PLIST_BASENAME = "com.bing-wallpaper-daily-mac-multimonitor"
LAUNCHD_PATH = Path.home() / "Library" / "LaunchAgents"
DEFAULT_PATH = "/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin"
BING_ARCHIVE_URL = "https://www.bing.com/HPImageArchive.aspx"
USER_AGENT = f"bing-wallpaper-daily-mac-multimonitor/{__version__}"
METADATA_TIMEOUT = 30
IMAGE_TIMEOUT = 60


class WallpaperError(Exception):
    """Raised when the wallpaper workflow fails."""


@dataclass(slots=True)
class Settings:
    proto: str
    country: Optional[str]
    day: int
    picture_dir: Path
    auto_update_name: str
    monitor: int
    force: bool
    quiet: bool
    experimental: bool
    filename: Optional[str]

    @property
    def plist_filename(self) -> Path:
        return LAUNCHD_PATH / f"{PLIST_BASENAME}-{self.auto_update_name}.plist"

    @property
    def plist_label(self) -> str:
        return f"{PLIST_BASENAME}.{self.auto_update_name}"


def normalize_auto_update_name(name: str) -> str:
    cleaned = name.strip() or "default"
    return "".join(ch if ch.isalnum() or ch in "-_" else "-" for ch in cleaned)


def log(message: str, quiet: bool) -> None:
    if quiet:
        return
    timestamp = dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    print(f"{timestamp}: {message}")


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Download the Bing daily wallpaper and apply it to macOS desktops."
    )
    parser.add_argument(
        "command",
        nargs="?",
        choices=["enable-auto-update", "disable-auto-update", "info"],
        help="One-shot commands for launchd setup or showing the current wallpaper info.",
    )
    parser.add_argument(
        "--auto-update-name",
        default="default",
        help="Name of the auto-update job (use different names for multiple configurations).",
    )
    parser.add_argument(
        "-f", "--force", action="store_true", help="Force download even if the file already exists."
    )

    ssl_group = parser.add_mutually_exclusive_group()
    ssl_group.set_defaults(ssl=True)
    ssl_group.add_argument(
        "-s", "--ssl", dest="ssl", action="store_true", help="Download images over HTTPS (default)."
    )
    ssl_group.add_argument(
        "--no-ssl", dest="ssl", action="store_false", help="Use HTTP instead of HTTPS."
    )

    parser.add_argument(
        "-q", "--quiet", action="store_true", help="Suppress log messages."
    )
    parser.add_argument(
        "-c",
        "--country",
        help="Market country/region (e.g. en-US, cs-CZ).",
    )
    parser.add_argument(
        "-d",
        "--day",
        type=int,
        default=0,
        help="Day offset (0=today, 1=yesterday, ...).",
    )
    parser.add_argument(
        "-n",
        "--filename",
        help="Custom filename for the downloaded image (extension is optional).",
    )
    parser.add_argument(
        "-p",
        "--picturedir",
        type=Path,
        default=DEFAULT_PICTURE_DIR,
        help=f"Directory to save wallpapers [default: {DEFAULT_PICTURE_DIR}]",
    )
    parser.add_argument(
        "-r",
        "--resolution",
        help="Single resolution to try (overrides the default resolution list).",
    )
    parser.add_argument(
        "--resolutions",
        nargs="+",
        help="List of resolutions to try in order (e.g. --resolutions 1920x1200 1920x1080 UHD).",
    )
    parser.add_argument(
        "-m",
        "--monitor",
        type=int,
        default=0,
        help="Set wallpaper only on a specific monitor (1, 2, 3...). Defaults to all monitors.",
    )
    parser.add_argument(
        "--all-desktops-experimental",
        action="store_true",
        help=(
            "Set wallpaper across all desktops by writing to desktoppicture.db. "
            "Known issue: minimized apps are removed from Dock."
        ),
    )
    parser.add_argument(
        "--version", action="version", version=__version__
    )
    return parser.parse_args(argv)


def build_archive_url(day: int, country: Optional[str]) -> str:
    query = {"format": "xml", "idx": str(day), "n": "1"}
    if country:
        query["mkt"] = country
    return f"{BING_ARCHIVE_URL}?{urllib.parse.urlencode(query)}"


def fetch_image_metadata(archive_url: str) -> tuple[str, bytes]:
    try:
        request = urllib.request.Request(archive_url, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request, timeout=METADATA_TIMEOUT) as response:
            if getattr(response, "status", 200) != 200:
                raise WallpaperError(f"Unexpected status {response.status} from {archive_url}")
            body = response.read()
    except HTTPError as exc:  # pragma: no cover - network failures need to be surfaced
        raise WallpaperError(f"Unable to fetch Bing metadata from {archive_url}: HTTP {exc.code}") from exc
    except Exception as exc:  # pragma: no cover - network failures need to be surfaced
        raise WallpaperError(f"Unable to fetch Bing metadata from {archive_url}") from exc

    try:
        root = ET.fromstring(body)
        url_base = root.findtext(".//urlBase")
    except ET.ParseError as exc:
        raise WallpaperError("Could not parse Bing metadata response.") from exc

    if not url_base:
        raise WallpaperError("Bing response did not include an image URL.")
    return url_base, body


def sanitize_filename(name: str) -> str:
    trimmed = name.strip()
    if not trimmed:
        return "wallpaper.jpg"
    basename = Path(trimmed).name
    if not Path(basename).suffix:
        basename = f"{basename}.jpg"
    return basename


def download_image(
    *,
    url_base: str,
    resolution: str,
    settings: Settings,
    metadata_body: bytes,
) -> Tuple[Optional[Path], bool]:
    file_url_with_res = f"{url_base}_{resolution}.jpg"
    file_url = f"{settings.proto}://www.bing.com/{file_url_with_res.lstrip('/')}"

    if settings.filename:
        filename_local = sanitize_filename(settings.filename)
    else:
        filename_local = file_url_with_res.replace("/th?id=", "")
    filename_local = f"{settings.auto_update_name}-{filename_local}"
    target_path = settings.picture_dir / filename_local

    if target_path.exists() and not settings.force:
        log(f"Skipping download, already present: {target_path.name}", settings.quiet)
        return target_path, True

    log(f"Downloading {resolution} from {file_url}", settings.quiet)
    temp_path = target_path.with_suffix(f"{target_path.suffix}.tmp")
    try:
        temp_path.unlink(missing_ok=True)
        file_request = urllib.request.Request(file_url, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(file_request, timeout=IMAGE_TIMEOUT) as response:
            if getattr(response, "status", 200) != 200:
                raise WallpaperError(f"Unexpected status {response.status} when fetching {file_url}")
            with temp_path.open("wb") as handle:
                handle.write(response.read())

        # Remove previous downloads only after the new one is safely on disk
        for candidate in settings.picture_dir.glob(f"{settings.auto_update_name}-*.jpg"):
            if candidate != target_path:
                candidate.unlink(missing_ok=True)

        temp_path.replace(target_path)

        info_path = settings.picture_dir / "info.xml"
        with info_path.open("wb") as handle:
            handle.write(metadata_body)
    except HTTPError as exc:
        temp_path.unlink(missing_ok=True)
        raise WallpaperError(f"Failed to download wallpaper at {resolution}: HTTP {exc.code}") from exc
    except Exception as exc:
        # Clean up partial downloads
        temp_path.unlink(missing_ok=True)
        raise WallpaperError(f"Failed to download wallpaper at {resolution}") from exc

    return target_path, False


def set_wallpaper(file_path: Path, monitor: int, quiet: bool) -> None:
    posix_path = file_path.as_posix().replace('"', '\\"')
    if monitor >= 1:
        script = f"""
        set tlst to {{}}
        tell application "System Events"
            set tlst to a reference to every desktop
            set picture of item {monitor} of tlst to (POSIX file "{posix_path}")
        end tell
        """
    else:
        script = (
            f'tell application "System Events" to tell every desktop to set picture to (POSIX file "{posix_path}")'
        )

    log(f"Setting wallpaper to {file_path} (monitor: {'all' if monitor < 1 else monitor})", quiet)
    subprocess.run(["osascript", "-e", script], check=True)


def set_wallpaper_experimental(file_path: Path, quiet: bool) -> None:
    db_path = Path.home() / "Library" / "Application Support" / "Dock" / "desktoppicture.db"
    if not db_path.exists():
        raise WallpaperError(f"desktoppicture.db not found at {db_path}")

    log("Writing wallpaper to desktoppicture.db (experimental all desktops)", quiet)
    conn = sqlite3.connect(db_path)
    try:
        with conn:
            conn.execute("insert into data values (?)", (str(file_path),))
            new_entry = conn.execute("select max(rowid) from data;").fetchone()[0]
            pictures = [row[0] for row in conn.execute("select rowid from pictures;").fetchall()]
            conn.execute("delete from preferences;")
            for pic in pictures:
                conn.execute(
                    "insert into preferences (key, data_id, picture_id) values(1, ?, ?)",
                    (new_entry, pic),
                )
    finally:
        conn.close()

    try:
        subprocess.run(["killall", "Dock"], check=True)
    except subprocess.CalledProcessError as exc:
        raise WallpaperError("Failed to restart Dock after updating wallpaper.") from exc


def create_launchd_plist(settings: Settings, rest_args: Sequence[str]) -> None:
    LAUNCHD_PATH.mkdir(parents=True, exist_ok=True)

    # Ensure the enable-auto-update marker is removed from ProgramArguments
    filtered_args = list(rest_args)
    if "enable-auto-update" in filtered_args:
        filtered_args.remove("enable-auto-update")

    program_arguments = [sys.executable, "-m", "bing_wallpaper.cli", *filtered_args]

    plist_data = {
        "Label": settings.plist_label,
        "OnDemand": True,
        "ProgramArguments": program_arguments,
        "EnvironmentVariables": {"PATH": DEFAULT_PATH},
        "StandardErrorPath": f"/tmp/{PLIST_BASENAME}-{settings.auto_update_name}.err",
        "StandardOutPath": f"/tmp/{PLIST_BASENAME}-{settings.auto_update_name}.out",
        "StartInterval": 1800,
        "RunAtLoad": True,
    }

    with settings.plist_filename.open("wb") as handle:
        import plistlib  # Imported lazily to keep startup fast

        plistlib.dump(plist_data, handle)

    subprocess.run(["launchctl", "unload", "-w", str(settings.plist_filename)], check=False)
    subprocess.run(["launchctl", "load", "-w", str(settings.plist_filename)], check=True)


def remove_launchd_plist(settings: Settings) -> None:
    subprocess.run(["launchctl", "unload", "-w", str(settings.plist_filename)], check=False)
    settings.plist_filename.unlink(missing_ok=True)


def show_info(picture_dir: Path) -> None:
    info_path = picture_dir / "info.xml"
    if not info_path.exists():
        raise WallpaperError(f"No info.xml found in {picture_dir}. Run the download first.")

    try:
        root = ET.parse(info_path).getroot()
        headline = root.findtext(".//headline", default="")
        copyright_text = root.findtext(".//copyright", default="Unknown copyright")
        copyrightlink = root.findtext(".//copyrightlink", default="")
    except ET.ParseError as exc:
        raise WallpaperError("Failed to parse info.xml") from exc

    info = copyright_text

    if headline:
        info = f"{headline}\n{info}"
    if copyrightlink:
        info = f"{info}\n{copyrightlink}"

    print(info)


def ensure_picture_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)


def main(argv: Optional[Sequence[str]] = None) -> int:
    raw_args = list(sys.argv[1:] if argv is None else argv)
    args = parse_args(raw_args)

    resolutions: List[str]
    if args.resolution and args.resolutions:
        raise WallpaperError("Provide either --resolution or --resolutions, not both.")
    if args.resolution:
        resolutions = [args.resolution]
    elif args.resolutions:
        resolutions = args.resolutions
    else:
        resolutions = DEFAULT_RESOLUTIONS

    settings = Settings(
        proto="https" if args.ssl else "http",
        country=args.country,
        day=args.day,
        picture_dir=args.picturedir.expanduser(),
        auto_update_name=normalize_auto_update_name(args.auto_update_name or "default"),
        monitor=args.monitor,
        force=args.force,
        quiet=args.quiet,
        experimental=args.all_desktops_experimental,
        filename=args.filename,
    )

    if args.command == "enable-auto-update":
        create_launchd_plist(settings, raw_args)
        log("Automatic wallpaper update enabled.", settings.quiet)
        return 0
    if args.command == "disable-auto-update":
        remove_launchd_plist(settings)
        log("Automatic wallpaper update disabled.", settings.quiet)
        return 0
    if args.command == "info":
        show_info(settings.picture_dir)
        return 0

    ensure_picture_dir(settings.picture_dir)

    archive_url = build_archive_url(settings.day, settings.country)
    url_base, metadata_body = fetch_image_metadata(archive_url)

    last_error: Optional[Exception] = None
    for res in resolutions:
        try:
            file_path, skipped = download_image(
                url_base=url_base,
                resolution=res,
                settings=settings,
                metadata_body=metadata_body,
            )
        except WallpaperError as exc:
            last_error = exc
            log(f"Resolution {res} failed: {exc}", settings.quiet)
            continue

        if file_path is None:
            continue

        try:
            if settings.experimental:
                if skipped:
                    log("Download skipped; experimental all-desktops update not applied.", settings.quiet)
                else:
                    set_wallpaper_experimental(file_path, settings.quiet)
            else:
                set_wallpaper(file_path, settings.monitor, settings.quiet)
            return 0
        except Exception as exc:  # pragma: no cover - integrates with macOS
            last_error = WallpaperError(str(exc))
            continue

    if last_error:
        raise last_error
    raise WallpaperError("Unable to download wallpaper for any resolution.")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except WallpaperError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)
