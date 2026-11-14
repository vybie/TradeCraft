use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    width: f32,
    height: f32,
    maximized: bool,
    position: Option<egui::Pos2>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            width: 1200.0,
            height: 760.0,
            maximized: false,
            position: None,
        }
    }
}

impl Geometry {
    fn observe(
        &mut self,
        size: Option<egui::Vec2>,
        maximized: Option<bool>,
        minimized: Option<bool>,
        position: Option<egui::Pos2>,
    ) {
        // A minimized window can report an empty size or lose its maximized flag.
        if minimized != Some(false) {
            return;
        }
        if let Some(maximized) = maximized {
            self.maximized = maximized;
            if !maximized {
                if let Some(pos) = position.filter(|p| p.x.is_finite() && p.y.is_finite() && p.x.abs() <= 32768.0 && p.y.abs() <= 32768.0) {
                    self.position = Some(pos);
                }
            }
            if !maximized
                && let Some(size) = size
                && valid_size(size.x, size.y)
            {
                self.width = size.x;
                self.height = size.y;
            }
        }
    }
}

fn valid_size(width: f32, height: f32) -> bool {
    width.is_finite()
        && height.is_finite()
        && (640.0..=16384.0).contains(&width)
        && (480.0..=16384.0).contains(&height)
}

fn path() -> Option<PathBuf> {
    Some(
        tradecraft_providers::settings::data_dir()?
            .join(if cfg!(feature = "vibestrader-credential") {
                "window-personal.json"
            } else {
                "window-community.json"
            }),
    )
}

pub(crate) struct WindowSettings {
    geometry: Geometry,
    dirty_since: Option<Instant>,
}

impl WindowSettings {
    pub(crate) fn load() -> Self {
        let geometry = path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
            .and_then(|v| {
                let width = v["width"].as_f64()? as f32;
                let height = v["height"].as_f64()? as f32;
                valid_size(width, height).then_some(Geometry {
                    width,
                    height,
                    maximized: v["maximized"].as_bool()?,
                    position: v["x"].as_f64().zip(v["y"].as_f64()).and_then(|(x, y)| {
                        let (x, y) = (x as f32, y as f32);
                        (x.is_finite() && y.is_finite() && x.abs() <= 32768.0 && y.abs() <= 32768.0).then_some(egui::pos2(x, y))
                    }),
                })
            })
            .unwrap_or_default();
        Self {
            geometry,
            dirty_since: None,
        }
    }

    pub(crate) fn viewport(&self) -> egui::ViewportBuilder {
        let mut viewport = egui::ViewportBuilder::default()
            .with_inner_size([self.geometry.width, self.geometry.height])
            .with_maximized(self.geometry.maximized);
        if let Some(position) = self.geometry.position { viewport = viewport.with_position(position); }
        viewport
    }

    pub(crate) fn observe(&mut self, ctx: &egui::Context) {
        let before = self.geometry;
        ctx.input(|i| {
            let viewport = i.viewport();
            self.geometry.observe(
                viewport.inner_rect.map(|r| r.size()),
                viewport.maximized,
                viewport.minimized,
                viewport.outer_rect.map(|rect| rect.min),
            );
        });
        if self.geometry != before {
            self.dirty_since = Some(Instant::now());
        }
        if self
            .dirty_since
            .is_some_and(|when| when.elapsed() >= Duration::from_millis(500))
        {
            let _ = self.flush();
        }
    }

    pub(crate) fn flush(&mut self) -> Result<(), String> {
        if self.dirty_since.is_none() {
            return Ok(());
        }
        let path = path().ok_or("Window settings folder is unavailable")?;
        let parent = path.parent().ok_or("Window settings folder is unavailable")?;
        let data = serde_json::json!({"width": self.geometry.width, "height": self.geometry.height, "maximized": self.geometry.maximized, "x": self.geometry.position.map(|p| p.x), "y": self.geometry.position.map(|p| p.y)});
        std::fs::create_dir_all(parent).map_err(|e| format!("Could not save window settings: {e}"))?;
        crate::terminal_storage::write_atomic(&path, &data.to_string()).map_err(|e| format!("Could not save window settings: {e}"))?;
        self.dirty_since = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remembers_normal_size_and_maximized_state_through_minimization() {
        let mut geometry = Geometry::default();
        geometry.observe(Some(egui::vec2(1400.0, 900.0)), Some(false), Some(false), Some(egui::pos2(100.0, 200.0)));
        geometry.observe(Some(egui::vec2(1920.0, 1080.0)), Some(true), Some(false), Some(egui::pos2(0.0, 0.0)));
        geometry.observe(Some(egui::vec2(0.0, 0.0)), Some(false), Some(true), None);
        assert_eq!(
            geometry,
            Geometry {
                width: 1400.0,
                height: 900.0,
                maximized: true,
                position: Some(egui::pos2(100.0, 200.0))
            }
        );
    }
}
