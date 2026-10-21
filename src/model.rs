use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoder {
    #[default]
    Auto,
    Software,
    Nvenc,
}

impl Encoder {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Software => "HEVC (x265)",
            Self::Nvenc => "NVIDIA (NVENC)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizeMode {
    #[default]
    Bounds,
    Height,
    Percent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResizeOptions {
    pub enabled: bool,
    pub mode: ResizeMode,
    pub width: u32,
    pub height: u32,
    pub percent: f64,
    pub height_percent: f64,
    pub keep_aspect: bool,
}
impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ResizeMode::Bounds,
            width: 1280,
            height: 720,
            percent: 50.0,
            height_percent: 50.0,
            keep_aspect: true,
        }
    }
}
impl ResizeOptions {
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        match self.mode {
            ResizeMode::Bounds => ensure!(
                (2..=16384).contains(&self.width) && (2..=16384).contains(&self.height),
                "Resize bounds must be 2–16384 pixels."
            ),
            ResizeMode::Height => ensure!(
                (2..=16384).contains(&self.height),
                "Resize height must be 2–16384 pixels."
            ),
            ResizeMode::Percent => ensure!(
                self.percent.is_finite()
                    && self.height_percent.is_finite()
                    && (0.1..=100.0).contains(&self.percent)
                    && (0.1..=100.0).contains(&self.height_percent),
                "Resize percentages must be 0.1–100%."
            ),
        }
        Ok(())
    }
    /// Display-oriented pixel dimensions. Both axes are always minification-only.
    pub fn dimensions(&self, width: u32, height: u32) -> (u32, u32) {
        if !self.enabled {
            return (width, height);
        }
        let (mut x, mut y) = match self.mode {
            ResizeMode::Bounds => (
                (self.width as f64 / width.max(1) as f64).min(1.0),
                (self.height as f64 / height.max(1) as f64).min(1.0),
            ),
            ResizeMode::Height => (1.0, (self.height as f64 / height.max(1) as f64).min(1.0)),
            ResizeMode::Percent => (
                self.percent / 100.0,
                if self.keep_aspect {
                    self.percent / 100.0
                } else {
                    self.height_percent / 100.0
                },
            ),
        };
        if self.keep_aspect {
            x = x.min(y);
            y = x;
        }
        fn even(value: f64, maximum: u32) -> u32 {
            ((value.floor().max(2.0) as u32 / 2) * 2).min(maximum)
        }
        (
            even(width as f64 * x, width),
            even(height as f64 * y, height),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub start: f64,
    pub end: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub resize: bool,
    pub resize_mode: ResizeMode,
    pub keep_aspect: bool,
    pub percent: f64,
    pub height_percent: f64,
    pub quality: u8,
    pub encoder: Encoder,
    pub output_dir: PathBuf,
    pub replace_original: bool,
    pub delete_original: bool,
    pub force: bool,
    #[serde(alias = "keep_larger")]
    pub force_overwrite: bool,
    pub compare: bool,
    pub min_ssim: Option<f64>,
    pub auto_trim: Option<crate::detect::DetectionSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: None,
            width: 1280,
            height: 720,
            resize: false,
            resize_mode: ResizeMode::Bounds,
            keep_aspect: true,
            percent: 50.0,
            height_percent: 50.0,
            quality: 21,
            encoder: Encoder::Auto,
            output_dir: PathBuf::new(),
            replace_original: true,
            delete_original: true,
            force: false,
            force_overwrite: false,
            compare: false,
            min_ssim: None,
            auto_trim: None,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.start.is_finite() && self.start >= 0.0,
            "Start must be a non-negative time."
        );
        if let Some(end) = self.end {
            ensure!(
                end.is_finite() && end > self.start,
                "End must be after start."
            );
        }
        ensure!(
            (15..=30).contains(&self.quality),
            "Quality must be between 15 and 30."
        );
        self.resize_options().validate()?;
        ensure!(
            self.replace_original || !self.output_dir.as_os_str().is_empty(),
            "Choose an output folder."
        );
        if let Some(value) = self.min_ssim {
            ensure!(
                value.is_finite() && (0.0..=1.0).contains(&value),
                "SSIM must be between 0 and 1."
            );
        }
        if let Some(settings) = &self.auto_trim {
            settings.validate()?;
        }
        Ok(())
    }

    pub fn resize_options(&self) -> ResizeOptions {
        ResizeOptions {
            enabled: self.resize,
            mode: self.resize_mode,
            width: self.width,
            height: self.height,
            percent: self.percent,
            height_percent: self.height_percent,
            keep_aspect: self.keep_aspect,
        }
    }

    pub fn range(&self, duration: f64, detected: Option<f64>) -> Result<(f64, f64)> {
        let end = self
            .end
            .unwrap_or(duration)
            .min(duration)
            .min(detected.unwrap_or(duration));
        ensure!(
            end - self.start >= 0.05,
            "The trim range is empty or outside this video's duration."
        );
        Ok((self.start, end))
    }
}

pub fn parse_time(text: &str) -> Result<f64> {
    let parts: Vec<_> = text.trim().split(':').collect();
    ensure!(
        !parts.is_empty() && parts.len() <= 3,
        "Use seconds or HH:MM:SS.mmm."
    );
    let mut seconds = 0.0;
    for (i, part) in parts.iter().enumerate() {
        ensure!(
            !part.is_empty() && part.chars().all(|c| c.is_ascii_digit() || c == '.'),
            "Use seconds or HH:MM:SS.mmm."
        );
        let value: f64 = part.parse()?;
        ensure!(
            value.is_finite() && value >= 0.0,
            "Time must be non-negative."
        );
        if i > 0 {
            ensure!(value < 60.0, "Minutes and seconds must be less than 60.");
        }
        if i + 1 != parts.len() && value.fract() != 0.0 {
            bail!("Only seconds may contain a fraction.");
        }
        seconds = seconds * 60.0 + value;
    }
    Ok(seconds)
}

pub fn time_text(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1000 % 60,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_round_trips() {
        for value in [0.0, 12.125, 3723.25, 100000.5] {
            assert!((parse_time(&time_text(value)).unwrap() - value).abs() < 0.001);
        }
        assert_eq!(parse_time("1:02:03").unwrap(), 3723.0);
        for bad in ["NaN", "-1", "1:60", "1.5:02", "", "1::2"] {
            assert!(parse_time(bad).is_err());
        }
    }
    #[test]
    fn batch_ranges_clamp_and_reject_short_files() {
        let settings = Settings {
            start: 10.0,
            end: Some(100.0),
            ..Default::default()
        };
        assert_eq!(settings.range(50.0, Some(30.0)).unwrap(), (10.0, 30.0));
        assert!(settings.range(5.0, None).is_err());
    }
    #[test]
    fn resize_modes_share_minification_and_even_pixel_planning() {
        let mut resize = ResizeOptions {
            enabled: true,
            mode: ResizeMode::Height,
            height: 540,
            ..Default::default()
        };
        assert_eq!(resize.dimensions(1920, 1080), (960, 540));
        resize.keep_aspect = false;
        assert_eq!(resize.dimensions(1920, 1080), (1920, 540));
        resize.mode = ResizeMode::Percent;
        resize.keep_aspect = true;
        resize.percent = 25.0;
        assert_eq!(resize.dimensions(1920, 1080), (480, 270));
        resize.keep_aspect = false;
        resize.height_percent = 50.0;
        assert_eq!(resize.dimensions(1920, 1080), (480, 540));
        resize.mode = ResizeMode::Bounds;
        resize.width = 3840;
        resize.height = 2160;
        assert_eq!(resize.dimensions(1920, 1080), (1920, 1080));
        resize.mode = ResizeMode::Percent;
        resize.percent = 101.0;
        assert!(resize.validate().is_err());
    }
}
