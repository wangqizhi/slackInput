#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Ring,
    Solid,
    Crosshair,
    X,
}

impl Style {
    pub const ALL: [Self; 4] = [Self::Ring, Self::Solid, Self::Crosshair, Self::X];

    pub fn key(self) -> &'static str {
        match self {
            Self::Ring => "ring",
            Self::Solid => "solid",
            Self::Crosshair => "crosshair",
            Self::X => "x",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "solid" => Self::Solid,
            "crosshair" => Self::Crosshair,
            "x" => Self::X,
            _ => Self::Ring,
        }
    }
}

/// Shared pixel geometry keeps the preview and native overlay identical.
pub fn spans(diameter: u8, style: Style) -> Vec<(i32, i32, i32)> {
    let radius = f32::from(diameter) / 2.0;
    let filled = |x: i32, y: i32| {
        let dx = x as f32 + 0.5 - radius;
        let dy = y as f32 + 0.5 - radius;
        let distance = dx * dx + dy * dy;
        distance <= radius * radius
            && (distance >= (radius - 2.0).powi(2)
                || match style {
                    Style::Ring => false,
                    Style::Solid => true,
                    Style::Crosshair => dx.abs() <= 1.0 || dy.abs() <= 1.0,
                    Style::X => (dx.abs() - dy.abs()).abs() <= 1.2,
                })
    };
    let mut spans = Vec::new();
    for y in 0..i32::from(diameter) {
        let mut x = 0;
        while x < i32::from(diameter) {
            if filled(x, y) {
                let start = x;
                while x < i32::from(diameter) && filled(x, y) {
                    x += 1;
                }
                spans.push((start, y, x - start));
            } else {
                x += 1;
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn styles_preserve_transparency_and_distinct_interior_shapes() {
        let contains = |style, x, y| {
            spans(24, style)
                .iter()
                .any(|&(start, row, length)| row == y && x >= start && x < start + length)
        };
        assert!(!contains(Style::Ring, 12, 12));
        assert!(contains(Style::Solid, 15, 12));
        assert!(contains(Style::Crosshair, 15, 12));
        assert!(!contains(Style::Crosshair, 15, 15));
        assert!(contains(Style::X, 15, 15));
        assert!(!contains(Style::X, 15, 12));
        for style in Style::ALL {
            assert!(!contains(style, 0, 0));
        }
    }
}
