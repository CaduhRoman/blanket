//! Vector icons drawn with the egui painter (no image assets to decode or keep in
//! memory), and the procedurally rasterized application icon.

use std::f32::consts::{PI, TAU};

use egui::epaint::{CubicBezierShape, PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Back,
    Forward,
    Reload,
    Home,
    Close,
    Plus,
    Star,
    StarFilled,
    Lock,
    Warning,
    Info,
    Download,
    Menu,
    History,
    Bookmarks,
    Settings,
    Gauge,
    Moon,
    Speaker,
    Search,
    Globe,
    Folder,
    Check,
    Puzzle,
}

/// Maps a 16×16 design grid into `rect`.
struct Grid {
    origin: Pos2,
    unit: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        let origin = rect.center() - vec2(side, side) / 2.0;
        Self {
            origin,
            unit: side / 16.0,
        }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + vec2(x, y) * self.unit
    }

    fn len(&self, value: f32) -> f32 {
        value * self.unit
    }
}

pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let g = Grid::new(rect);
    let stroke = Stroke::new(g.len(1.45).max(1.1), color);
    let line = |points: &[(f32, f32)]| {
        painter.add(PathShape::line(
            points.iter().map(|&(x, y)| g.p(x, y)).collect(),
            PathStroke::new(stroke.width, color),
        ));
    };
    let closed = |points: &[(f32, f32)], fill: Color32| {
        painter.add(PathShape::convex_polygon(
            points.iter().map(|&(x, y)| g.p(x, y)).collect(),
            fill,
            if fill == Color32::TRANSPARENT { stroke } else { Stroke::NONE },
        ));
    };
    let arc = |center: (f32, f32), radius: f32, from: f32, to: f32| {
        let steps = 24;
        let points = (0..=steps)
            .map(|i| {
                let angle = from + (to - from) * i as f32 / steps as f32;
                g.p(center.0 + radius * angle.cos(), center.1 + radius * angle.sin())
            })
            .collect();
        painter.add(PathShape::line(points, PathStroke::new(stroke.width, color)));
    };

    match icon {
        Icon::Back => {
            line(&[(9.5, 3.5), (5.0, 8.0), (9.5, 12.5)]);
        },
        Icon::Forward => {
            line(&[(6.5, 3.5), (11.0, 8.0), (6.5, 12.5)]);
        },
        Icon::Reload => {
            arc((8.0, 8.0), 4.6, -PI * 0.35, PI * 1.45);
            let tip = (8.0 + 4.6 * (-PI * 0.35).cos(), 8.0 + 4.6 * (-PI * 0.35).sin());
            closed(&[(tip.0 + 1.9, tip.1 - 1.4), (tip.0 + 0.4, tip.1 + 2.1), (tip.0 - 1.7, tip.1 - 0.9)], color);
        },
        Icon::Home => {
            line(&[(2.8, 7.6), (8.0, 3.0), (13.2, 7.6)]);
            line(&[(4.5, 6.4), (4.5, 13.0), (11.5, 13.0), (11.5, 6.4)]);
            line(&[(7.0, 13.0), (7.0, 9.6), (9.0, 9.6), (9.0, 13.0)]);
        },
        Icon::Close => {
            line(&[(4.5, 4.5), (11.5, 11.5)]);
            line(&[(11.5, 4.5), (4.5, 11.5)]);
        },
        Icon::Plus => {
            line(&[(8.0, 3.5), (8.0, 12.5)]);
            line(&[(3.5, 8.0), (12.5, 8.0)]);
        },
        Icon::Star | Icon::StarFilled => {
            let points: Vec<Pos2> = (0..10)
                .map(|i| {
                    let radius = if i % 2 == 0 { 5.6 } else { 2.5 };
                    let angle = -PI / 2.0 + i as f32 * PI / 5.0;
                    g.p(8.0 + radius * angle.cos(), 8.4 + radius * angle.sin())
                })
                .collect();
            if icon == Icon::StarFilled {
                // The star is concave; fill it as a fan of triangles around the center.
                let center = g.p(8.0, 8.4);
                for i in 0..10 {
                    painter.add(PathShape::convex_polygon(
                        vec![center, points[i], points[(i + 1) % 10]],
                        color,
                        Stroke::new(0.5, color),
                    ));
                }
            } else {
                painter.add(PathShape::closed_line(points, PathStroke::new(stroke.width, color)));
            }
        },
        Icon::Lock => {
            painter.rect_stroke(
                Rect::from_min_max(g.p(4.0, 7.2), g.p(12.0, 13.2)),
                g.len(1.6),
                stroke,
                egui::StrokeKind::Middle,
            );
            arc((8.0, 7.2), 2.5, PI, TAU);
            painter.circle_filled(g.p(8.0, 10.2), g.len(0.9), color);
        },
        Icon::Warning => {
            painter.add(PathShape::closed_line(
                vec![g.p(8.0, 2.6), g.p(14.0, 13.2), g.p(2.0, 13.2)],
                PathStroke::new(stroke.width, color),
            ));
            line(&[(8.0, 6.4), (8.0, 9.6)]);
            painter.circle_filled(g.p(8.0, 11.5), g.len(0.8), color);
        },
        Icon::Info => {
            painter.circle_stroke(g.p(8.0, 8.0), g.len(5.6), stroke);
            line(&[(8.0, 7.2), (8.0, 11.2)]);
            painter.circle_filled(g.p(8.0, 5.1), g.len(0.8), color);
        },
        Icon::Download => {
            line(&[(8.0, 2.8), (8.0, 10.0)]);
            line(&[(4.8, 7.0), (8.0, 10.2), (11.2, 7.0)]);
            line(&[(3.2, 13.0), (12.8, 13.0)]);
        },
        Icon::Menu => {
            for x in [3.6, 8.0, 12.4] {
                painter.circle_filled(g.p(x, 8.0), g.len(1.15), color);
            }
        },
        Icon::History => {
            arc((8.0, 8.0), 5.4, -PI * 0.9, PI * 0.75);
            line(&[(8.0, 5.0), (8.0, 8.3), (10.4, 9.8)]);
            line(&[(1.8, 4.6), (2.9, 7.0), (5.2, 5.8)]);
        },
        Icon::Bookmarks => {
            line(&[(4.5, 2.8), (11.5, 2.8), (11.5, 13.4), (8.0, 10.6), (4.5, 13.4), (4.5, 2.8)]);
        },
        Icon::Settings => {
            painter.circle_stroke(g.p(8.0, 8.0), g.len(2.1), stroke);
            for i in 0..8 {
                let angle = i as f32 * TAU / 8.0;
                let (sin, cos) = angle.sin_cos();
                painter.line_segment(
                    [g.p(8.0 + 4.0 * cos, 8.0 + 4.0 * sin), g.p(8.0 + 5.9 * cos, 8.0 + 5.9 * sin)],
                    Stroke::new(stroke.width * 1.5, color),
                );
            }
            painter.circle_stroke(g.p(8.0, 8.0), g.len(4.3), stroke);
        },
        Icon::Gauge => {
            arc((8.0, 9.5), 5.5, PI, TAU);
            line(&[(8.0, 9.5), (11.0, 6.2)]);
            painter.circle_filled(g.p(8.0, 9.5), g.len(1.0), color);
            line(&[(2.5, 9.5), (2.5, 12.6), (13.5, 12.6), (13.5, 9.5)]);
        },
        Icon::Moon => {
            arc((8.0, 8.0), 5.2, PI * 0.25, PI * 1.75);
            painter.add(CubicBezierShape::from_points_stroke(
                [g.p(11.7, 4.3), g.p(8.4, 5.2), g.p(8.4, 10.8), g.p(11.7, 11.7)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        },
        Icon::Speaker => {
            closed(&[(2.5, 6.0), (5.0, 6.0), (8.5, 3.2), (8.5, 12.8), (5.0, 10.0), (2.5, 10.0)], color);
            arc((8.6, 8.0), 3.2, -PI * 0.3, PI * 0.3);
            arc((8.6, 8.0), 5.4, -PI * 0.3, PI * 0.3);
        },
        Icon::Search => {
            painter.circle_stroke(g.p(7.0, 7.0), g.len(4.0), stroke);
            line(&[(10.0, 10.0), (13.2, 13.2)]);
        },
        Icon::Globe => {
            painter.circle_stroke(g.p(8.0, 8.0), g.len(5.6), stroke);
            line(&[(2.4, 8.0), (13.6, 8.0)]);
            painter.add(CubicBezierShape::from_points_stroke(
                [g.p(8.0, 2.4), g.p(4.8, 5.0), g.p(4.8, 11.0), g.p(8.0, 13.6)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
            painter.add(CubicBezierShape::from_points_stroke(
                [g.p(8.0, 2.4), g.p(11.2, 5.0), g.p(11.2, 11.0), g.p(8.0, 13.6)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        },
        Icon::Folder => {
            line(&[(2.5, 4.0), (6.2, 4.0), (7.6, 5.6), (13.5, 5.6), (13.5, 12.6), (2.5, 12.6), (2.5, 4.0)]);
        },
        Icon::Check => {
            line(&[(3.5, 8.4), (6.6, 11.4), (12.6, 4.8)]);
        },
        Icon::Puzzle => {
            painter.rect_stroke(
                Rect::from_min_max(g.p(3.0, 4.0), g.p(12.0, 13.0)),
                g.len(1.4),
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.circle_filled(g.p(7.5, 4.0), g.len(1.6), color);
            painter.circle_filled(g.p(12.0, 8.5), g.len(1.6), color);
        },
    }
}

/// Indeterminate spinner (a rotating arc). The caller requests repaints.
pub fn spinner(painter: &Painter, rect: Rect, color: Color32, time: f64) {
    let g = Grid::new(rect);
    let start = (time * 5.0) as f32 % TAU;
    let points = (0..=20)
        .map(|i| {
            let angle = start + i as f32 / 20.0 * PI * 1.4;
            g.p(8.0 + 5.0 * angle.cos(), 8.0 + 5.0 * angle.sin())
        })
        .collect();
    painter.add(PathShape::line(points, PathStroke::new(g.len(1.8).max(1.4), color)));
}

pub use crate::app_icon::app_icon_rgba;

/// Point helper for callers that lay out icons manually.
pub fn icon_rect(center: Pos2, side: f32) -> Rect {
    Rect::from_center_size(center, vec2(side, side))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_comes_from_the_logo_with_transparent_corners() {
        let (pixels, size) = app_icon_rgba(64);
        assert_eq!(pixels.len(), (size * size * 4) as usize);
        assert_eq!(pixels[3], 0, "corner is transparent");
        let center = ((32 * size + 32) * 4 + 3) as usize;
        assert!(pixels[center] > 200, "center is (nearly) opaque: {}", pixels[center]);
    }
}
