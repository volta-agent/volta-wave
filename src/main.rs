use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use kittyaudio::{Mixer, Sound};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use serde::Deserialize;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use walkdir::WalkDir;

// ============================================================================
// PLAYLIST FORMAT
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Playlist {
    name: String,
    tracks: Vec<String>, // paths as strings
}

impl Playlist {
    fn new(name: String) -> Self {
        Self {
            name,
            tracks: Vec::new(),
        }
    }
    
    fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)
    }
    
    fn load(path: &PathBuf) -> std::io::Result<Self> {
        let json = fs::read_to_string(path)?;
        serde_json::from_str(&json).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

// ============================================================================
// LRCLIB API (via curl)
// ============================================================================

#[derive(Debug, Deserialize)]
struct LrcLibSearchResult {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
}

fn fetch_lyrics(artist: &str, title: &str) -> Option<String> {
    let url = format!(
        "https://lrclib.net/api/search?artist_name={}&track_name={}",
        url_encode(artist),
        url_encode(title)
    );

    let output = Command::new("curl")
        .arg("-s")
        .arg("-S")
        .arg("--max-time")
        .arg("5")
        .arg(&url)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let results: Vec<LrcLibSearchResult> = serde_json::from_slice(&output.stdout).ok()?;
    results.into_iter().filter_map(|r| r.synced_lyrics).next()
}

fn url_encode(s: &str) -> String {
    let mut encoded = String::new();
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => encoded.push(c),
            ' ' => encoded.push('+'),
            _ => {
                for byte in c.to_string().as_bytes() {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    encoded
}

// ============================================================================
// SYNCED LYRICS
// ============================================================================

#[derive(Clone, Debug)]
struct LyricLine {
    time_ms: u64,
    text: String,
}

#[derive(Clone)]
struct SyncedLyrics {
    lines: Vec<LyricLine>,
}

impl SyncedLyrics {
    fn parse(lrc: &str) -> Option<Self> {
        let mut lines = Vec::new();
        for line in lrc.lines() {
            if line.starts_with('[') && line.contains(']') {
                let end_bracket = line.find(']')?;
                let time_str = &line[1..end_bracket];
                if let Some(colon_pos) = time_str.find(':') {
                    let mins: u64 = time_str[..colon_pos].parse().ok()?;
                    let rest = &time_str[colon_pos + 1..];
                    let secs: f64 = rest.parse().ok()?;
                    let time_ms = mins * 60 * 1000 + (secs * 1000.0) as u64;
                    let text = line[end_bracket + 1..].trim().to_string();
                    lines.push(LyricLine { time_ms, text });
                }
            }
        }
        if lines.is_empty() {
            None
        } else {
            lines.sort_by_key(|l| l.time_ms);
            Some(Self { lines })
        }
    }

    fn get_line_at(&self, time_ms: u64) -> Option<(usize, &str)> {
        if self.lines.is_empty() {
            return None;
        }
        let mut idx = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if line.time_ms <= time_ms {
                idx = i;
            } else {
                break;
            }
        }
        Some((idx, &self.lines[idx].text))
    }
}

// ============================================================================
// TRACK
// ============================================================================

#[derive(Clone)]
struct Track {
    path: PathBuf,
    title: String,
    artist: String,
    lyrics_path: PathBuf,
}

impl Track {
    fn from_path(path: PathBuf) -> Self {
        let filename = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
            .to_string();

        let (artist, title) = if let Some(pos) = filename.find(" - ") {
            (filename[..pos].to_string(), filename[pos + 3..].to_string())
        } else {
            ("Unknown".to_string(), filename)
        };

        let lyrics_path = path.with_extension("lrc");

        Self {
            path,
            title,
            artist,
            lyrics_path,
        }
    }

    fn load_lyrics(&self) -> Option<String> {
        if self.lyrics_path.exists() {
            fs::read_to_string(&self.lyrics_path).ok()
        } else {
            if let Some(lrc) = fetch_lyrics(&self.artist, &self.title) {
                let _ = fs::write(&self.lyrics_path, &lrc);
                Some(lrc)
            } else {
                None
            }
        }
    }
}

// ============================================================================
// APP MODE
// ============================================================================

#[derive(Clone, Copy, PartialEq)]
enum AppMode {
    Normal,       // Default track list view
    Browser,      // File browser
    PlaylistMenu, // Load/save playlist menu
}

// ============================================================================
// PLAYLIST MENU STATE
// ============================================================================

struct PlaylistMenu {
    playlists: Vec<String>,
    selected: usize,
    is_saving: bool,
    input_buffer: String,
}

impl PlaylistMenu {
    fn new() -> Self {
        Self {
            playlists: Vec::new(),
            selected: 0,
            is_saving: false,
            input_buffer: String::new(),
        }
    }
 
    fn refresh(&mut self, playlists: Vec<String>) {
        self.playlists = playlists;
        if self.selected >= self.playlists.len() {
            self.selected = 0;
        }
    }
}

// ============================================================================
// VISUALIZATION MODES
// ============================================================================

#[derive(Clone, Copy, PartialEq)]
enum VizMode {
    Spectrum,      // Classic bar spectrum
    Wave,          // Sine wave pattern
    Circles,       // Concentric circles
    Stars,         // Twinkling stars
    Mirror,        // Mirrored spectrum
}

impl VizMode {
    fn next(self) -> Self {
        match self {
            VizMode::Spectrum => VizMode::Wave,
            VizMode::Wave => VizMode::Circles,
            VizMode::Circles => VizMode::Stars,
            VizMode::Stars => VizMode::Mirror,
            VizMode::Mirror => VizMode::Spectrum,
        }
    }
    
    fn name(self) -> &'static str {
        match self {
            VizMode::Spectrum => "Spectrum",
            VizMode::Wave => "Wave",
            VizMode::Circles => "Circles",
            VizMode::Stars => "Stars",
            VizMode::Mirror => "Mirror",
        }
    }
}

// ============================================================================
// FILE BROWSER
// ============================================================================

#[derive(Clone)]
struct BrowserEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

struct FileBrowser {
    current_dir: PathBuf,
    entries: Vec<BrowserEntry>,
    selected: usize,
}

impl FileBrowser {
    fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let mut browser = Self {
            current_dir: PathBuf::from(home),
            entries: Vec::new(),
            selected: 0,
        };
        browser.refresh();
        browser
    }
    
    fn refresh(&mut self) {
        self.entries.clear();
        
        // Parent directory entry
        if self.current_dir.parent().is_some() {
            self.entries.push(BrowserEntry {
                name: "..".to_string(),
                path: self.current_dir.parent().unwrap().to_path_buf(),
                is_dir: true,
            });
        }
        
        // Read directory contents
        if let Ok(entries) = fs::read_dir(&self.current_dir) {
            let mut dirs: Vec<BrowserEntry> = Vec::new();
            let mut files: Vec<BrowserEntry> = Vec::new();
            
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                
                let is_dir = path.is_dir();
                let is_mp3 = path.extension()
                    .map(|ext| ext.eq_ignore_ascii_case("mp3"))
                    .unwrap_or(false);
                
                if is_dir {
                    dirs.push(BrowserEntry { name, path, is_dir: true });
                } else if is_mp3 {
                    files.push(BrowserEntry { name, path, is_dir: false });
                }
            }
            
            // Sort and combine: directories first, then files
            dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            
            self.entries.extend(dirs);
            self.entries.extend(files);
        }
        
        // Ensure selection is valid
        if self.selected >= self.entries.len() {
            self.selected = 0;
        }
    }
    
    fn enter_selected(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?;
        
        if entry.is_dir {
            self.current_dir = entry.path.clone();
            self.refresh();
            None
        } else {
            Some(entry.path.clone())
        }
    }
    
    fn go_up(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.refresh();
        }
    }
}

// ============================================================================
// APP STATE
// ============================================================================

struct App {
    tracks: Vec<Track>,
    selected: usize,
    playing: Option<usize>,
    mixer: Mixer,
    sound_handle: Option<kittyaudio::SoundHandle>,
    spectrum: Vec<f32>,
    wave_phase: f32,
    lyrics: Option<SyncedLyrics>,
    quitting: bool,
    show_help: bool,
    sample_rate: u32,
    viz_mode: VizMode,
    mode: AppMode,
    browser: FileBrowser,
    playlist_menu: PlaylistMenu,
    playlist_dir: PathBuf,
    status_msg: Option<String>,
}

impl App {
    fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let playlist_dir = PathBuf::from(&home).join(".volta-wave/playlists");
        
        Self {
            tracks: Vec::new(),
            selected: 0,
            playing: None,
            mixer: Mixer::new(),
            sound_handle: None,
            spectrum: vec![0.0; 32],
            wave_phase: 0.0,
            lyrics: None,
            quitting: false,
            show_help: false,
            sample_rate: 44100,
            viz_mode: VizMode::Spectrum,
            mode: AppMode::Normal,
            browser: FileBrowser::new(),
            playlist_menu: PlaylistMenu::new(),
            playlist_dir,
            status_msg: None,
        }
    }

    fn add_track(&mut self, path: PathBuf) {
        // Check if already in playlist
        if self.tracks.iter().any(|t| t.path == path) {
            self.status_msg = Some("Track already in playlist".to_string());
            return;
        }
        
        let track = Track::from_path(path);
        self.tracks.push(track);
        self.tracks.sort_by(|a, b| a.artist.cmp(&b.artist).then(a.title.cmp(&b.title)));
        self.status_msg = Some("Track added".to_string());
    }

    fn add_directory(&mut self, dir: PathBuf) {
        let mut added = 0;
        for entry in WalkDir::new(&dir)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext.eq_ignore_ascii_case("mp3")).unwrap_or(false))
        {
            let path = entry.path().to_path_buf();
            if !self.tracks.iter().any(|t| t.path == path) {
                self.tracks.push(Track::from_path(path));
                added += 1;
            }
        }
        self.tracks.sort_by(|a, b| a.artist.cmp(&b.artist).then(a.title.cmp(&b.title)));
        self.status_msg = Some(format!("Added {} tracks", added));
    }

    fn save_playlist(&mut self, name: &str) {
        let _ = fs::create_dir_all(&self.playlist_dir);
        let path = self.playlist_dir.join(format!("{}.json", name));
        
        let playlist = Playlist {
            name: name.to_string(),
            tracks: self.tracks.iter().map(|t| t.path.to_string_lossy().to_string()).collect(),
        };
        
        match playlist.save(&path) {
            Ok(_) => self.status_msg = Some(format!("Saved playlist: {}", name)),
            Err(e) => self.status_msg = Some(format!("Error saving: {}", e)),
        }
    }

    fn load_playlist(&mut self, name: &str) {
        let path = self.playlist_dir.join(format!("{}.json", name));
        
        match Playlist::load(&path) {
            Ok(playlist) => {
                self.tracks.clear();
                for track_path in playlist.tracks {
                    let p = PathBuf::from(&track_path);
                    if p.exists() {
                        self.tracks.push(Track::from_path(p));
                    }
                }
                self.selected = 0;
                self.playing = None;
                self.stop();
                self.status_msg = Some(format!("Loaded playlist: {}", name));
            }
            Err(e) => self.status_msg = Some(format!("Error loading: {}", e)),
        }
    }

    fn list_playlists(&self) -> Vec<String> {
        let mut playlists = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.playlist_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                        playlists.push(name.to_string());
                    }
                }
            }
        }
        playlists.sort();
        playlists
    }

    fn load_music(&mut self, music_dir: &str) {
        let music_path = PathBuf::from(music_dir);
        if music_path.exists() {
            for entry in WalkDir::new(music_path)
                .max_depth(2)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|ext| ext == "mp3").unwrap_or(false))
            {
                self.tracks.push(Track::from_path(entry.path().to_path_buf()));
            }
        }
        self.tracks
            .sort_by(|a, b| a.artist.cmp(&b.artist).then(a.title.cmp(&b.title)));
    }

    fn play(&mut self, index: usize) {
        self.stop();

        let track = &self.tracks[index];

        // Load lyrics first
        if let Some(lrc) = track.load_lyrics() {
            self.lyrics = SyncedLyrics::parse(&lrc);
        } else {
            self.lyrics = None;
        }

        // Initialize mixer if needed
        self.mixer.init();

        // Load and play audio
        match Sound::from_path(&track.path) {
            Ok(sound) => {
                self.sample_rate = sound.sample_rate();
                let handle = self.mixer.play(sound);
                self.sound_handle = Some(handle);
                self.playing = Some(index);
            }
            Err(e) => {
                eprintln!("Failed to load audio: {:?}", e);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(handle) = &self.sound_handle {
            // Seek to end so it finishes immediately and gets removed from mixer
            handle.guard().seek_to_end();
        }
        self.sound_handle = None;
        self.playing = None;
        self.lyrics = None;
    }

    fn is_playing(&self) -> bool {
        self.sound_handle
            .as_ref()
            .map(|h| !h.finished() && !h.paused())
            .unwrap_or(false)
    }

    fn current_time_ms(&self) -> u64 {
        if let Some(ref handle) = self.sound_handle {
            let guard = handle.guard();
            let index = guard.index();
            let sample_rate = guard.sample_rate();
            // Convert sample index to milliseconds
            if sample_rate > 0 {
                (index as u64 * 1000) / sample_rate as u64
            } else {
                0
            }
        } else {
            0
        }
    }
}

// ============================================================================
// UI RENDERING
// ============================================================================

fn draw_visualization(f: &mut Frame, area: Rect, spectrum: &[f32], phase: f32, mode: VizMode) {
    if spectrum.is_empty() || area.width < 2 || area.height < 2 {
        return;
    }

    match mode {
        VizMode::Spectrum => draw_spectrum_viz(f, area, spectrum, phase),
        VizMode::Wave => draw_wave_viz(f, area, spectrum, phase),
        VizMode::Circles => draw_circles_viz(f, area, spectrum, phase),
        VizMode::Stars => draw_stars_viz(f, area, spectrum, phase),
        VizMode::Mirror => draw_mirror_viz(f, area, spectrum, phase),
    }

    let title = format!(" {} [v to change] ", mode.name());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.as_str())
        .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);
}

fn draw_spectrum_viz(f: &mut Frame, area: Rect, spectrum: &[f32], phase: f32) {
    let bar_width = (area.width.saturating_sub(2)) / spectrum.len() as u16;
    if bar_width == 0 {
        return;
    }

    for (i, &val) in spectrum.iter().enumerate() {
        let height = ((area.height.saturating_sub(2) as f32) * val).max(1.0) as u16;
        let x = area.x + 1 + i as u16 * bar_width;

        let hue = (i as f32 / spectrum.len() as f32 * 360.0 + phase * 57.0) % 360.0;
        let color = hsv_to_rgb(hue, 0.8, 0.9);

        for y in 0..height {
            let y_pos = area.y + area.height.saturating_sub(2) - y;
            if y_pos <= area.y || y_pos >= area.y + area.height {
                break;
            }
            f.render_widget(
                Paragraph::new("█").style(Style::default().fg(color)),
                Rect::new(x, y_pos, bar_width.saturating_sub(1).max(1), 1),
            );
        }
    }
}

fn draw_wave_viz(f: &mut Frame, area: Rect, spectrum: &[f32], phase: f32) {
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let center_y = area.y + 1 + (height / 2) as u16;

    for x in 0..width {
        let x_pos = area.x + 1 + x as u16;
        
        // Combine multiple sine waves
        let wave1 = (x as f32 * 0.1 + phase).sin();
        let wave2 = (x as f32 * 0.05 + phase * 0.7).sin();
        let wave3 = (x as f32 * 0.15 + phase * 1.3).sin();
        
        let amp = (spectrum.iter().sum::<f32>() / spectrum.len() as f32).max(0.3);
        let combined = (wave1 + wave2 * 0.5 + wave3 * 0.3) / 1.8 * amp;
        
        let y_offset = (combined * height as f32 * 0.4) as i16;
        let y_pos = (center_y as i16 + y_offset) as u16;
        
        if y_pos > area.y && y_pos < area.y + area.height.saturating_sub(1) {
            let hue = (x as f32 / width as f32 * 360.0 + phase * 30.0) % 360.0;
            let color = hsv_to_rgb(hue, 0.8, 0.9);
            f.render_widget(
                Paragraph::new("●").style(Style::default().fg(color)),
                Rect::new(x_pos, y_pos, 1, 1),
            );
        }
    }
}

fn draw_circles_viz(f: &mut Frame, area: Rect, spectrum: &[f32], phase: f32) {
    let center_x = area.x + area.width / 2;
    let center_y = area.y + area.height / 2;
    let max_radius = (area.width.min(area.height) / 2).saturating_sub(2) as f32;
    
    for (i, &val) in spectrum.iter().enumerate() {
        let base_radius = (i as f32 / spectrum.len() as f32) * max_radius;
        let radius = base_radius + val * max_radius * 0.3;
        let radius = radius.max(1.0) as u16;
        
        let hue = (i as f32 / spectrum.len() as f32 * 360.0 + phase * 40.0) % 360.0;
        let color = hsv_to_rgb(hue, 0.7, 0.9);
        
        // Draw circle points
        let num_points = (radius as f32 * 6.28 * 2.0) as usize;
        for p in 0..num_points.max(8) {
            let angle = (p as f32 / num_points as f32) * 6.28 + phase + i as f32 * 0.1;
            let dx = (angle.cos() * radius as f32) as i16;
            let dy = (angle.sin() * radius as f32 * 0.5) as i16; // Flatten for aspect ratio
            
            let x = (center_x as i16 + dx) as u16;
            let y = (center_y as i16 + dy) as u16;
            
            if x > area.x && x < area.x + area.width.saturating_sub(1) 
               && y > area.y && y < area.y + area.height.saturating_sub(1) {
                f.render_widget(
                    Paragraph::new("·").style(Style::default().fg(color)),
                    Rect::new(x, y, 1, 1),
                );
            }
        }
    }
}

fn draw_stars_viz(f: &mut Frame, area: Rect, spectrum: &[f32], phase: f32) {
    use rand::{Rng, SeedableRng};
    
    // Use phase as a seed for deterministic randomness per frame
    let seed = phase as u64;
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let width = area.width.saturating_sub(2);
    let height = area.height.saturating_sub(2);
    
    let num_stars = 50;
    for _ in 0..num_stars {
        let x = area.x + 1 + rng.gen_range(0..width);
        let y = area.y + 1 + rng.gen_range(0..height);
        
        let brightness = spectrum.iter().sum::<f32>() / spectrum.len() as f32;
        let twinkle = ((phase * 3.0 + x as f32 * 0.1 + y as f32 * 0.1).sin() * 0.5 + 0.5) * brightness;
        
        let hue = rng.gen_range(0.0..60.0) + 200.0; // Blue-ish
        let color = hsv_to_rgb(hue, 0.3 + twinkle * 0.5, 0.5 + twinkle * 0.5);
        
        let chars = ["·", "✦", "✧", "★", "☆"];
        let char_idx = (twinkle * (chars.len() - 1) as f32) as usize;
        let char_idx = char_idx.min(chars.len() - 1);
        
        f.render_widget(
            Paragraph::new(chars[char_idx]).style(Style::default().fg(color)),
            Rect::new(x, y, 1, 1),
        );
    }
    
    // Draw a few larger "shooting stars"
    if spectrum.iter().sum::<f32>() / spectrum.len() as f32 > 0.4 {
        for _ in 0..3 {
            let start_x = area.x + 1 + rng.gen_range(0..width / 2);
            let start_y = area.y + 1 + rng.gen_range(0..height / 2);
            let len = rng.gen_range(3..8);
            
            for i in 0..len {
                let x = start_x + i;
                let y = start_y + i / 2;
                if x < area.x + area.width.saturating_sub(1) && y < area.y + area.height.saturating_sub(1) {
                    let alpha = 1.0 - i as f32 / len as f32;
                    let color = hsv_to_rgb(45.0, 0.2, alpha);
                    f.render_widget(
                        Paragraph::new("-").style(Style::default().fg(color)),
                        Rect::new(x, y, 1, 1),
                    );
                }
            }
        }
    }
}

fn draw_mirror_viz(f: &mut Frame, area: Rect, spectrum: &[f32], phase: f32) {
    let bar_width = (area.width.saturating_sub(2)) / (spectrum.len() * 2) as u16;
    if bar_width == 0 {
        return;
    }
    
    let center_x = area.x + area.width / 2;
    let center_y = area.y + area.height / 2;
    
    for (i, &val) in spectrum.iter().enumerate() {
        let height = ((area.height.saturating_sub(2) as f32) / 2.0 * val).max(1.0) as u16;
        
        // Right side
        let x_right = center_x + i as u16 * bar_width;
        let hue = (i as f32 / spectrum.len() as f32 * 180.0 + phase * 57.0) % 360.0;
        let color = hsv_to_rgb(hue, 0.8, 0.9);
        
        for y in 0..height {
            let y_top = center_y.saturating_sub(y);
            let y_bot = center_y + y;
            
            if y_top > area.y {
                f.render_widget(
                    Paragraph::new("█").style(Style::default().fg(color)),
                    Rect::new(x_right, y_top, bar_width.saturating_sub(1).max(1), 1),
                );
            }
            if y_bot < area.y + area.height.saturating_sub(1) {
                f.render_widget(
                    Paragraph::new("█").style(Style::default().fg(color)),
                    Rect::new(x_right, y_bot, bar_width.saturating_sub(1).max(1), 1),
                );
            }
        }
        
        // Left side (mirrored)
        let x_left = center_x.saturating_sub((i + 1) as u16 * bar_width);
        
        for y in 0..height {
            let y_top = center_y.saturating_sub(y);
            let y_bot = center_y + y;
            
            if y_top > area.y {
                f.render_widget(
                    Paragraph::new("█").style(Style::default().fg(color)),
                    Rect::new(x_left, y_top, bar_width.saturating_sub(1).max(1), 1),
                );
            }
            if y_bot < area.y + area.height.saturating_sub(1) {
                f.render_widget(
                    Paragraph::new("█").style(Style::default().fg(color)),
                    Rect::new(x_left, y_bot, bar_width.saturating_sub(1).max(1), 1),
                );
            }
        }
    }
}

fn draw_lyrics(f: &mut Frame, area: Rect, lyrics: &Option<SyncedLyrics>, current_time_ms: u64) {
    let height = area.height.saturating_sub(2) as usize;

    if height == 0 {
        let block = Block::default().borders(Borders::ALL).title(" Lyrics ");
        f.render_widget(block, area);
        return;
    }

    let lines_to_show = match lyrics {
        Some(synced) => {
            let (current_idx, _) = synced.get_line_at(current_time_ms).unwrap_or((0, ""));

            let start = if current_idx > height / 2 {
                current_idx - height / 2
            } else {
                0
            };

            synced
                .lines
                .iter()
                .skip(start)
                .take(height)
                .enumerate()
                .map(|(i, line)| {
                    let is_current = start + i == current_idx;
                    let style = if is_current {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let text = if line.text.is_empty() {
                        "♪".to_string()
                    } else {
                        line.text.clone()
                    };
                    Line::styled(text, style)
                })
                .collect::<Vec<_>>()
        }
        None => {
            vec![Line::styled(
                "No lyrics available",
                Style::default().fg(Color::DarkGray),
            )]
        }
    };

    let paragraph = Paragraph::new(lines_to_show)
        .wrap(Wrap { trim: false })
        .alignment(ratatui::layout::Alignment::Center);

    let inner = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    f.render_widget(paragraph, inner);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Lyrics ")
        .title_style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(block, area);
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let h = h % 360.0;
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    Color::Rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

fn ui(f: &mut Frame, app: &mut App) {
    match app.mode {
        AppMode::Normal => ui_normal(f, app),
        AppMode::Browser => ui_browser(f, app),
        AppMode::PlaylistMenu => ui_playlist_menu(f, app),
    }
    
    // Status bar at bottom
    let status = if let Some(ref msg) = app.status_msg {
        msg.clone()
    } else if let Some(playing_idx) = app.playing {
        let track = &app.tracks[playing_idx];
        let time_s = app.current_time_ms() / 1000;
        let mins = time_s / 60;
        let secs = time_s % 60;
        let lyric_status = if app.lyrics.is_some() { " | Lyrics" } else { "" };
        format!(
            "{} - {} | {:02}:{:02}{}",
            track.artist, track.title, mins, secs, lyric_status
        )
    } else {
        "Press h for help | a to add files | o to open playlist | s to save".to_string()
    };

    let status_bar = Paragraph::new(status).style(Style::default().fg(Color::DarkGray));
    f.render_widget(
        status_bar,
        Rect::new(0, f.size().height.saturating_sub(1), f.size().width, 1),
    );

    // Help overlay
    if app.show_help {
        draw_help_overlay(f);
    }
}

fn ui_normal(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(f.size());

    // Track list
    let tracks: Vec<ListItem> = app
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let style = if Some(i) == app.playing {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if i == app.selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::White)
            };

            let prefix = if Some(i) == app.playing && app.is_playing() {
                "▶ "
            } else if Some(i) == app.playing {
                "⏸ "
            } else {
                " "
            };

            let has_lyrics = track.lyrics_path.exists()
                || (Some(i) == app.playing && app.lyrics.is_some());
            let lyric_marker = if has_lyrics && Some(i) == app.playing {
                " 🎤"
            } else {
                ""
            };

            ListItem::new(format!(
                "{}{} - {}{}",
                prefix, track.artist, track.title, lyric_marker
            ))
            .style(style)
        })
        .collect();

    let track_list = List::new(tracks).block(
        Block::default()
            .title(" Tracks ")
            .borders(Borders::ALL)
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
    );
    f.render_widget(track_list, chunks[0]);

    // Right side: spectrum + lyrics
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    draw_visualization(f, right_chunks[0], &app.spectrum, app.wave_phase, app.viz_mode);
    draw_lyrics(
        f,
        right_chunks[1],
        &app.lyrics,
        app.current_time_ms(),
    );
}

fn ui_browser(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(f.size());

    // File browser (left)
    let entries: Vec<ListItem> = app
        .browser
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let style = if i == app.browser.selected {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };

            let icon = if entry.is_dir { "📁 " } else { "🎵 " };
            ListItem::new(format!("{}{}", icon, entry.name)).style(style)
        })
        .collect();

    let browser_list = List::new(entries).block(
        Block::default()
            .title(format!(" Browse: {} ", app.browser.current_dir.display()))
            .borders(Borders::ALL)
            .title_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(browser_list, chunks[0]);

    // Current playlist (right)
    let playlist_items: Vec<ListItem> = app
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let style = if Some(i) == app.playing {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("{} - {}", track.artist, track.title)).style(style)
        })
        .collect();

    let playlist_list = List::new(playlist_items).block(
        Block::default()
            .title(format!(" Playlist ({} tracks) ", app.tracks.len()))
            .borders(Borders::ALL)
            .title_style(Style::default().fg(Color::Magenta)),
    );
    f.render_widget(playlist_list, chunks[1]);

    // Instructions
    let instructions = Paragraph::new("Enter: Add file/folder | Esc: Back | d: Add directory")
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(
        instructions,
        Rect::new(chunks[0].x, chunks[0].y + chunks[0].height, chunks[0].width, 1),
    );
}

fn ui_playlist_menu(f: &mut Frame, app: &mut App) {
    let area = Rect::new(
        f.size().width / 4,
        f.size().height / 4,
        f.size().width / 2,
        f.size().height / 2,
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    // Playlist list
    let items: Vec<ListItem> = app
        .playlist_menu
        .playlists
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if i == app.playlist_menu.selected {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(name.as_str()).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Playlists (Enter to load) ")
            .borders(Borders::ALL)
            .title_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(list, chunks[0]);

    // Input for saving
    let input_title = if app.playlist_menu.is_saving {
        " Save as: (type name, Enter to save) "
    } else {
        " Press 's' to save, Esc to cancel "
    };

    let input_text = if app.playlist_menu.is_saving {
        app.playlist_menu.input_buffer.as_str()
    } else {
        ""
    };

    let input = Paragraph::new(input_text).block(
        Block::default()
            .title(input_title)
            .borders(Borders::ALL)
            .title_style(Style::default().fg(Color::Magenta)),
    );
    f.render_widget(input, chunks[1]);
}

fn draw_help_overlay(f: &mut Frame) {
    let help_text = r#"
  j/k or ↑/↓   Navigate tracks
  Enter         Play selected
  s             Stop
  n             Next track
  p             Previous track
  Space         Pause/Resume
  v             Cycle visualization
  a             Add files (browser)
  o             Open playlist menu
  d             Delete from playlist
  h             Toggle this help
  q             Quit

  Lyrics auto-fetched from
  lrclib.net and saved as .lrc
        "#;

    let help_popup = Paragraph::new(help_text).block(
        Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .title_style(Style::default().fg(Color::Yellow)),
);

    let area = Rect::new(
        f.size().width / 4,
        f.size().height / 4,
        f.size().width / 2,
        f.size().height / 2,
    );
    f.render_widget(help_popup, area);
}

// ============================================================================
// KEY EVENT HANDLING
// ============================================================================

fn handle_key_event(app: &mut App, key: crossterm::event::KeyEvent) {
    match app.mode {
        AppMode::Normal => handle_normal_mode(app, key),
        AppMode::Browser => handle_browser_mode(app, key),
        AppMode::PlaylistMenu => handle_playlist_menu_mode(app, key),
    }
}

fn handle_normal_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => {
            app.quitting = true
        }

        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
            if !app.tracks.is_empty() {
                app.selected = (app.selected + 1) % app.tracks.len();
            }
        }
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
            if !app.tracks.is_empty() {
                app.selected = app.selected.saturating_sub(1);
            }
        }
        (_, KeyCode::Enter) => {
            if !app.tracks.is_empty() {
                app.play(app.selected);
            }
        }
        (_, KeyCode::Char('s')) => {
            app.stop();
        }
        (_, KeyCode::Char('n')) => {
            if let Some(current) = app.playing {
                let next = (current + 1) % app.tracks.len();
                app.play(next);
                app.selected = next;
            }
        }
        (_, KeyCode::Char('p')) => {
            if let Some(current) = app.playing {
                let prev = if current == 0 {
                    app.tracks.len() - 1
                } else {
                    current - 1
                };
                app.play(prev);
                app.selected = prev;
            }
        }
        (_, KeyCode::Char(' ')) => {
            if let Some(handle) = &app.sound_handle {
                if handle.paused() {
                    handle.resume();
                } else {
                    handle.pause();
                }
            }
        }
        (_, KeyCode::Char('v')) => {
            app.viz_mode = app.viz_mode.next();
        }
        (_, KeyCode::Char('h')) => {
            app.show_help = !app.show_help;
        }
        (_, KeyCode::Char('a')) => {
            // Open file browser
            app.mode = AppMode::Browser;
            app.browser.refresh();
        }
        (_, KeyCode::Char('o')) => {
            // Open playlist menu
            let playlists = app.list_playlists();
            app.playlist_menu.refresh(playlists);
            app.mode = AppMode::PlaylistMenu;
        }
        (_, KeyCode::Char('d')) => {
            // Delete selected track from playlist
            if !app.tracks.is_empty() {
                let was_playing = app.playing == Some(app.selected);
                app.tracks.remove(app.selected);
                if app.selected >= app.tracks.len() && !app.tracks.is_empty() {
                    app.selected = app.tracks.len() - 1;
                }
                if was_playing {
                    app.stop();
                }
                // Adjust playing index if needed
                if let Some(p) = app.playing {
                    if p > app.selected {
                        app.playing = Some(p - 1);
                    }
                }
                app.status_msg = Some("Track removed".to_string());
            }
        }
        _ => {}
    }
}

fn handle_browser_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match (key.modifiers, key.code) {
        (_, KeyCode::Esc) => {
            app.mode = AppMode::Normal;
        }
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
            if !app.browser.entries.is_empty() {
                app.browser.selected = (app.browser.selected + 1) % app.browser.entries.len();
            }
        }
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
            if !app.browser.entries.is_empty() {
                app.browser.selected = app.browser.selected.saturating_sub(1);
            }
        }
        (_, KeyCode::Enter) => {
            if let Some(path) = app.browser.enter_selected() {
                // Add single file
                app.add_track(path);
                app.mode = AppMode::Normal;
            }
        }
        (_, KeyCode::Char('d')) => {
            // Add directory
            if let Some(entry) = app.browser.entries.get(app.browser.selected) {
                if entry.is_dir && entry.name != ".." {
                    app.add_directory(entry.path.clone());
                    app.mode = AppMode::Normal;
                }
            }
        }
        (_, KeyCode::Char('h')) | (_, KeyCode::Left) => {
            app.browser.go_up();
        }
        (_, KeyCode::Char('l')) | (_, KeyCode::Right) => {
            let _ = app.browser.enter_selected();
        }
        _ => {}
    }
}

fn handle_playlist_menu_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match (key.modifiers, key.code) {
        (_, KeyCode::Esc) => {
            if app.playlist_menu.is_saving {
                app.playlist_menu.is_saving = false;
                app.playlist_menu.input_buffer.clear();
            } else {
                app.mode = AppMode::Normal;
            }
        }
        (_, KeyCode::Char('s')) if !app.playlist_menu.is_saving => {
            app.playlist_menu.is_saving = true;
        }
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
            if !app.playlist_menu.is_saving && !app.playlist_menu.playlists.is_empty() {
                app.playlist_menu.selected = (app.playlist_menu.selected + 1) % app.playlist_menu.playlists.len();
            }
        }
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
            if !app.playlist_menu.is_saving && !app.playlist_menu.playlists.is_empty() {
                app.playlist_menu.selected = app.playlist_menu.selected.saturating_sub(1);
            }
        }
        (_, KeyCode::Enter) => {
            if app.playlist_menu.is_saving {
                // Save playlist
                let name = app.playlist_menu.input_buffer.trim().to_string();
                if !name.is_empty() {
                    app.save_playlist(&name);
                    app.playlist_menu.is_saving = false;
                    app.playlist_menu.input_buffer.clear();
                    app.mode = AppMode::Normal;
                }
            } else if !app.playlist_menu.playlists.is_empty() {
                // Load playlist
                let name = app.playlist_menu.playlists[app.playlist_menu.selected].clone();
                app.load_playlist(&name);
                app.mode = AppMode::Normal;
            }
        }
        (_, KeyCode::Backspace) => {
            if app.playlist_menu.is_saving {
                app.playlist_menu.input_buffer.pop();
            }
        }
        (_, KeyCode::Char(c)) => {
            if app.playlist_menu.is_saving {
                app.playlist_menu.input_buffer.push(c);
            }
        }
        _ => {}
    }
}

// ============================================================================
// MAIN
// ============================================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.load_music(&format!("{}/Music", std::env::var("HOME").unwrap()));

    if app.tracks.is_empty() {
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        eprintln!("No MP3 files found in ~/Music/");
        return Ok(());
    }

    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(33);

    while !app.quitting {
        terminal.draw(|f| ui(f, &mut app))?;

        // Clear status message after a few seconds
        if app.status_msg.is_some() {
            app.status_msg = None;
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                handle_key_event(&mut app, key);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.wave_phase += 0.1;

            if app.is_playing() {
                for (i, val) in app.spectrum.iter_mut().enumerate() {
                    let base = (i as f32 / 32.0).sqrt() + 0.2;
                    let target = ((app.wave_phase * (i + 1) as f32 * 0.5).sin() * 0.4 + 0.5)
                        * base
                        * (1.0 + 0.3 * (app.wave_phase * 2.0 + i as f32 * 0.1).sin());
                    *val = *val * 0.6 + target * 0.4;
                }
        } else {
            for (i, val) in app.spectrum.iter_mut().enumerate() {
                let idle =
                    (app.wave_phase * 0.2 * (i as f32 * 0.1 + 1.0)).sin() * 0.05 + 0.05;
                *val = *val * 0.95 + idle * 0.05;
            }
        }

        last_tick = Instant::now();
    }
}

    app.stop();
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
