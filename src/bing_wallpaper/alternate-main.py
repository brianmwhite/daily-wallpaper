#!/usr/bin/env python3
"""
Bing Wallpaper Daily Mac Multimonitor
Download and set current Bing Daily Wallpaper automatically on all (or selected) monitors for macOS
"""

import os
import sys
import subprocess
import xml.etree.ElementTree as ET
import shutil
from pathlib import Path
from datetime import datetime
import click
import requests

VERSION = '2.0.0'
RESOLUTIONS = ['1920x1200', '1920x1080', '1024x768', '1280x720', '1366x768', 'UHD']


class BingWallpaper:
    def __init__(self, picture_dir=None, auto_update_name="default", quiet=False):
        self.picture_dir = Path(picture_dir) if picture_dir else Path.home() / "Pictures" / "bing-wallpapers"
        self.auto_update_name = auto_update_name
        self.quiet = quiet
        self.plist_file = Path.home() / "Library" / "LaunchAgents" / f"com.bing-wallpaper-daily-mac-multimonitor-{auto_update_name}.plist"
        
    def print_message(self, message):
        """Print message with timestamp if not quiet"""
        if not self.quiet:
            print(f"{datetime.now().strftime('%Y-%m-%d %H:%M:%S')}: {message}")
    
    def create_picture_dir(self):
        """Create picture directory if it doesn't exist"""
        self.picture_dir.mkdir(parents=True, exist_ok=True)
    
    def download_image(self, resolution, country=None, day=0, force=False, ssl=True):
        """Download image from Bing API"""
        protocol = "https" if ssl else "http"
        bing_url = f"https://www.bing.com/HPImageArchive.aspx?format=xml&idx={day}&n=1"
        
        if country:
            bing_url += f"&mkt={country}"
        
        try:
            # Get image metadata
            response = requests.get(bing_url, timeout=30)
            response.raise_for_status()
            
            # Parse XML response
            root = ET.fromstring(response.content)
            url_base = root.find('.//urlBase').text
            
            # Construct image URL
            file_url_with_res = f"{url_base}_{resolution}.jpg"
            filename = file_url_with_res.replace('/th?id=', '')
            filename_local = f"{self.auto_update_name}-{filename}"
            file_whole_url = f"{protocol}://bing.com/{file_url_with_res}"
            
            filepath = self.picture_dir / filename_local
            
            # Check if file exists and force is not set
            if not force and filepath.exists():
                self.print_message(f"Skipping download: {filename_local}...")
                return str(filepath), True  # filepath, download_skipped
            
            # Remove old wallpapers with same prefix
            for old_file in self.picture_dir.glob(f"{self.auto_update_name}-*.jpg"):
                old_file.unlink()
            
            # Download image
            self.print_message(f"Downloading: {filename}...")
            img_response = requests.get(file_whole_url, timeout=60)
            img_response.raise_for_status()
            
            # Save image
            with open(filepath, 'wb') as f:
                f.write(img_response.content)
            
            # Save metadata
            info_file = self.picture_dir / "info.xml"
            with open(info_file, 'wb') as f:
                f.write(response.content)
            
            return str(filepath), False  # filepath, download_skipped
            
        except requests.RequestException as e:
            self.print_message(f"Error downloading image: {e}")
            return None, False
        except ET.ParseError as e:
            self.print_message(f"Error parsing XML response: {e}")
            return None, False
        except Exception as e:
            self.print_message(f"Unexpected error: {e}")
            return None, False
    
    def set_wallpaper(self, filepath, monitor=0):
        """Set wallpaper using osascript"""
        try:
            if monitor >= 1:
                self.print_message(f"Setting wallpaper for monitor: {monitor}")
                applescript = f'''
                set tlst to {{}}
                tell application "System Events"
                    set tlst to a reference to every desktop
                    set picture of item {monitor} of tlst to "{filepath}"
                end tell
                '''
            else:
                applescript = f'''
                tell application "System Events"
                    tell every desktop
                        set picture to "{filepath}"
                    end tell
                end tell
                '''
            
            subprocess.run(['osascript', '-e', applescript], check=True)
            self.print_message("Wallpaper set successfully")
            
        except subprocess.CalledProcessError as e:
            self.print_message(f"Error setting wallpaper: {e}")
    
    def set_wallpaper_experimental(self, filepath):
        """Set wallpaper using experimental database method"""
        try:
            db_file = Path.home() / "Library" / "Application Support" / "Dock" / "desktoppicture.db"
            
            # Insert image path
            subprocess.run(['sqlite3', str(db_file), f'insert into data values("{filepath}");'], check=True)
            
            # Get new entry ID
            result = subprocess.run(['sqlite3', str(db_file), 'select max(rowid) from data;'], 
                                  capture_output=True, text=True, check=True)
            new_entry = result.stdout.strip()
            
            # Get picture IDs
            result = subprocess.run(['sqlite3', str(db_file), 'select rowid from pictures;'], 
                                  capture_output=True, text=True, check=True)
            pictures = result.stdout.strip().split('\n')
            
            # Clear preferences and set new ones
            sql_commands = ["delete from preferences;"]
            for pic in pictures:
                if pic:
                    sql_commands.append(f"insert into preferences (key, data_id, picture_id) values(1, {new_entry}, {pic});")
            
            sql = " ".join(sql_commands)
            subprocess.run(['sqlite3', str(db_file), sql], check=True)
            
            # Restart Dock
            subprocess.run(['killall', 'Dock'], check=True)
            
        except subprocess.CalledProcessError as e:
            self.print_message(f"Error setting wallpaper (experimental): {e}")
    
    def show_info(self):
        """Show copyright info from saved XML"""
        try:
            info_file = self.picture_dir / "info.xml"
            if not info_file.exists():
                self.print_message("No info file found. Download a wallpaper first.")
                return
            
            with open(info_file, 'r', encoding='utf-8') as f:
                content = f.read()
            
            root = ET.fromstring(content)
            
            copyright_elem = root.find('.//copyright')
            headline_elem = root.find('.//headline')
            url_elem = root.find('.//copyrightlink')

            info = ""
            
            if copyright_elem is not None and copyright_elem.text:
                info = copyright_elem.text.strip()
            if headline_elem is not None and headline_elem.text:
                info = f"{headline_elem.text.strip()}\n{info}"
            if url_elem is not None and url_elem.text:
                info = f"{info}\n{url_elem.text.strip()}"

            if info != "":
                print(info)
            else:
                self.print_message("No copyright information found")
                
        except Exception as e:
            self.print_message(f"Error reading info: {e}")
    
    def create_plist(self, script_path, args):
        """Create launchd plist file for automatic updates"""
        try:
            # Create LaunchAgents directory if it doesn't exist
            self.plist_file.parent.mkdir(parents=True, exist_ok=True)
            
            # Filter out enable-auto-update from args
            filtered_args = [arg for arg in args if arg != 'enable-auto-update']
            args_str = ' '.join(filtered_args)
            
            plist_content = f'''<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.bing-wallpaper-daily-mac-multimonitor-{self.auto_update_name}.plist</string>
    <key>OnDemand</key>
    <true/>
    <key>ProgramArguments</key>
    <array>
        <string>{sys.executable}</string>
        <string>{script_path}</string>
        {chr(10).join(f'        <string>{arg}</string>' for arg in filtered_args)}
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
    <key>StandardErrorPath</key>
    <string>/tmp/bing-wallpaper-daily-mac-multimonitor-{self.auto_update_name}.err</string>
    <key>StandardOutPath</key>
    <string>/tmp/bing-wallpaper-daily-mac-multimonitor-{self.auto_update_name}.out</string>
    <key>StartInterval</key>
    <integer>1800</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>'''
            
            # Write plist file
            with open(self.plist_file, 'w') as f:
                f.write(plist_content)
            
            # Unload existing and load new
            subprocess.run(['launchctl', 'unload', '-w', str(self.plist_file)], 
                         capture_output=True)
            subprocess.run(['launchctl', 'load', '-w', str(self.plist_file)], check=True)
            
            self.print_message(f"Automatic update enabled with name: {self.auto_update_name}")
            
        except Exception as e:
            self.print_message(f"Error creating plist: {e}")
    
    def remove_plist(self):
        """Remove launchd plist file to disable automatic updates"""
        try:
            if self.plist_file.exists():
                subprocess.run(['launchctl', 'unload', '-w', str(self.plist_file)], 
                             capture_output=True)
                self.plist_file.unlink()
                self.print_message(f"Automatic update disabled for: {self.auto_update_name}")
            else:
                self.print_message("No automatic update was configured")
                
        except Exception as e:
            self.print_message(f"Error removing plist: {e}")


@click.command()
@click.option('--version', is_flag=True, help='Show version')
@click.option('-f', '--force', is_flag=True, help='Force download of picture')
@click.option('-s', '--ssl', is_flag=True, default=True, help='Communicate with bing.com over SSL')
@click.option('-q', '--quiet', is_flag=True, help='Do not display log messages')
@click.option('-c', '--country', help='Specify market country/region eg. en-US, cs-CZ')
@click.option('-d', '--day', type=int, default=0, help='Day for which you want to get the picture (0=today, 1=yesterday, etc.)')
@click.option('-n', '--filename', help='The name of the downloaded picture')
@click.option('-p', '--picturedir', help='The full path to the picture download dir')
@click.option('-r', '--resolution', help=f'The resolution of the image to retrieve. Supported: {", ".join(RESOLUTIONS)}')
@click.option('--resolutions', help='The resolutions to try (space-separated)')
@click.option('-m', '--monitor', type=int, default=0, help='Set wallpaper only on certain monitor (1,2,3...)')
@click.option('--all-desktops-experimental', is_flag=True, help='Set wallpaper on all desktops (experimental)')
@click.option('--auto-update-name', default='default', help='Name of your auto update configuration')
@click.argument('action', required=False)
def main(version, force, ssl, quiet, country, day, filename, picturedir, resolution, resolutions, 
         monitor, all_desktops_experimental, auto_update_name, action):
    """Bing Wallpaper Daily Mac Multimonitor - Download and set Bing daily wallpapers"""
    
    if version:
        print(VERSION)
        return
    
    wallpaper = BingWallpaper(
        picture_dir=picturedir,
        auto_update_name=auto_update_name,
        quiet=quiet
    )
    
    # Handle special actions
    if action == 'enable-auto-update':
        script_path = os.path.abspath(__file__)
        args = [arg for arg in sys.argv[1:] if arg != 'enable-auto-update']
        wallpaper.create_plist(script_path, args)
        return
    
    if action == 'disable-auto-update':
        wallpaper.remove_plist()
        return
    
    if action == 'info':
        wallpaper.show_info()
        return
    
    # Create picture directory
    wallpaper.create_picture_dir()
    
    # Determine resolutions to try
    if resolution:
        resolutions_to_try = [resolution]
    elif resolutions:
        resolutions_to_try = resolutions.split()
    else:
        resolutions_to_try = RESOLUTIONS
    
    # Try to download and set wallpaper
    for res in resolutions_to_try:
        filepath, download_skipped = wallpaper.download_image(
            resolution=res,
            country=country,
            day=day,
            force=force,
            ssl=ssl
        )
        
        if filepath:
            if all_desktops_experimental:
                if not download_skipped:
                    wallpaper.set_wallpaper_experimental(filepath)
            else:
                wallpaper.set_wallpaper(filepath, monitor)
            break
    
    if not filepath:
        wallpaper.print_message("Failed to download wallpaper")
        sys.exit(1)


if __name__ == '__main__':
    main()