//! A ground-plane **homography**: maps image points (frame fractions, 0..1) to a
//! metric ground plane (meters). With it, a track's pixel trajectory becomes a
//! real-world path, so the analytics layer can estimate **speed** (m/s → km/h).
//!
//! Calibration is intuitive: mark the four corners of a known rectangle on the
//! ground (e.g. a parking space, a driveway section) in the image and give its
//! real width × length; [`Homography::from_quad`] solves the 3×3 transform.
//!
//! Pure math (a hand-rolled 8×8 Gaussian solve) — no FFI, fully unit-testable.

use serde::{Deserialize, Serialize};

/// A 3×3 image→ground homography, row-major. `[8] == 1` by construction.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Homography {
    pub m: [f32; 9],
}

impl Homography {
    /// Build from four image points (frame fractions) that correspond to the
    /// corners of a real ground rectangle `width_m` × `height_m`, given in the
    /// order top-left, top-right, bottom-right, bottom-left → ground
    /// `(0,0),(W,0),(W,H),(0,H)`. Returns `None` if the points are degenerate
    /// (collinear / coincident) or the dimensions are non-positive.
    pub fn from_quad(img: [(f32, f32); 4], width_m: f32, height_m: f32) -> Option<Homography> {
        if !width_m.is_finite() || !height_m.is_finite() || width_m <= 0.0 || height_m <= 0.0 {
            return None;
        }
        let dst = [
            (0.0, 0.0),
            (width_m, 0.0),
            (width_m, height_m),
            (0.0, height_m),
        ];
        Self::from_correspondences(img, dst)
    }

    /// General 4-point DLT: solve the homography mapping `src[i] -> dst[i]`.
    pub fn from_correspondences(src: [(f32, f32); 4], dst: [(f32, f32); 4]) -> Option<Homography> {
        // The image quad must be a simple convex quadrilateral in consistent
        // winding order. A self-intersecting ("bowtie") order — e.g. the user
        // clicks the corners as a Z instead of a ring — is still full-rank, so
        // the linear solve would succeed and silently produce a homography that
        // maps interior points to finite garbage. Reject it up front.
        if !is_convex_quad(src) {
            return None;
        }
        // For each pair (x,y)->(X,Y), with h8 fixed to 1:
        //   h0 x + h1 y + h2 - h6 xX - h7 yX = X
        //   h3 x + h4 y + h5 - h6 xY - h7 yY = Y
        // Build the 8×8 system A·h = b (h = h0..h7) and solve it.
        let mut a = [[0.0f64; 8]; 8];
        let mut b = [0.0f64; 8];
        for i in 0..4 {
            let (x, y) = (src[i].0 as f64, src[i].1 as f64);
            let (cx, cy) = (dst[i].0 as f64, dst[i].1 as f64);
            let r0 = 2 * i;
            let r1 = 2 * i + 1;
            a[r0] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * cx, -y * cx];
            b[r0] = cx;
            a[r1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * cy, -y * cy];
            b[r1] = cy;
        }
        let h = solve8(a, b)?;
        Some(Homography {
            m: [
                h[0] as f32,
                h[1] as f32,
                h[2] as f32,
                h[3] as f32,
                h[4] as f32,
                h[5] as f32,
                h[6] as f32,
                h[7] as f32,
                1.0,
            ],
        })
    }

    /// Project an image point (frame fractions) to ground coordinates (meters).
    /// Returns `None` if the point maps to/behind the horizon (`w <= 0`).
    pub fn project(&self, p: (f32, f32)) -> Option<(f32, f32)> {
        let (x, y) = p;
        let w = self.m[6] * x + self.m[7] * y + self.m[8];
        // Reject points at or behind the horizon. The calibration corners all map
        // with w > 0 by construction (quad in front of the camera, h8 = 1), so an
        // in-front ground point has w > 0; w <= 0 is the vanishing line or beyond
        // and would yield a mirror-flipped garbage coordinate.
        if w <= 1e-9 {
            return None;
        }
        let gx = (self.m[0] * x + self.m[1] * y + self.m[2]) / w;
        let gy = (self.m[3] * x + self.m[4] * y + self.m[5]) / w;
        if gx.is_finite() && gy.is_finite() {
            Some((gx, gy))
        } else {
            None
        }
    }
}

/// Is `p` a simple (non-self-intersecting) convex quadrilateral? True iff every
/// consecutive edge turns the same way (all cross-products share a sign). This
/// rejects both the collinear/degenerate case (no non-zero turn → sign stays 0)
/// and the bowtie case (mixed signs), independent of clockwise/counter-clockwise
/// winding.
fn is_convex_quad(p: [(f32, f32); 4]) -> bool {
    let mut sign = 0i32;
    for i in 0..4 {
        let a = p[i];
        let b = p[(i + 1) % 4];
        let c = p[(i + 2) % 4];
        let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
        let s = if cross > 1e-9 {
            1
        } else if cross < -1e-9 {
            -1
        } else {
            0
        };
        if s != 0 {
            if sign == 0 {
                sign = s;
            } else if sign != s {
                return false;
            }
        }
    }
    sign != 0
}

/// Solve an 8×8 linear system `A·x = b` by Gaussian elimination with partial
/// pivoting. Returns `None` if the matrix is singular (degenerate quad).
#[allow(clippy::needless_range_loop)] // index math reads clearer for elimination
fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    const N: usize = 8;
    for col in 0..N {
        // Partial pivot: largest |value| in this column at/under the diagonal.
        let mut piv = col;
        for r in (col + 1)..N {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-12 {
            return None; // singular
        }
        a.swap(col, piv);
        b.swap(col, piv);
        // Eliminate below.
        for r in (col + 1)..N {
            let f = a[r][col] / a[col][col];
            if f != 0.0 {
                for c in col..N {
                    a[r][c] -= f * a[col][c];
                }
                b[r] -= f * b[col];
            }
        }
    }
    // Back-substitution.
    let mut x = [0.0f64; N];
    for i in (0..N).rev() {
        let mut s = b[i];
        for c in (i + 1)..N {
            s -= a[i][c] * x[c];
        }
        x[i] = s / a[i][i];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn axis_aligned_square_maps_to_rectangle() {
        // Image corners of a centered square map to a 10 m × 4 m ground rect.
        let img = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
        let h = Homography::from_quad(img, 10.0, 4.0).unwrap();
        // Corners land on the rectangle corners.
        let (x0, y0) = h.project((0.2, 0.2)).unwrap();
        assert!(close(x0, 0.0, 1e-3) && close(y0, 0.0, 1e-3));
        let (x1, y1) = h.project((0.8, 0.2)).unwrap();
        assert!(close(x1, 10.0, 1e-3) && close(y1, 0.0, 1e-3));
        let (x2, y2) = h.project((0.8, 0.8)).unwrap();
        assert!(close(x2, 10.0, 1e-3) && close(y2, 4.0, 1e-3));
        // Center maps to the rectangle center (affine for an axis-aligned quad).
        let (cx, cy) = h.project((0.5, 0.5)).unwrap();
        assert!(close(cx, 5.0, 1e-3) && close(cy, 2.0, 1e-3));
    }

    #[test]
    fn affine_distance_is_linear() {
        // For an axis-aligned (affine) mapping, equal image steps are equal
        // ground steps: 0.6 of image width (0.2->0.8) == 10 m.
        let img = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
        let h = Homography::from_quad(img, 10.0, 4.0).unwrap();
        let a = h.project((0.35, 0.5)).unwrap(); // 0.25 -> 2.5 m along x... check
        let b = h.project((0.65, 0.5)).unwrap();
        // 0.30 of image x-span (0.6) = half the 10 m width = 5 m.
        assert!(close((b.0 - a.0).abs(), 5.0, 1e-2));
    }

    #[test]
    fn perspective_trapezoid_round_trips_corners() {
        // A perspective trapezoid (far edge narrower) still hits its corners.
        let img = [(0.35, 0.30), (0.65, 0.30), (0.85, 0.80), (0.15, 0.80)];
        let h = Homography::from_quad(img, 6.0, 12.0).unwrap();
        for (p, want) in [
            ((0.35, 0.30), (0.0, 0.0)),
            ((0.65, 0.30), (6.0, 0.0)),
            ((0.85, 0.80), (6.0, 12.0)),
            ((0.15, 0.80), (0.0, 12.0)),
        ] {
            let g = h.project(p).unwrap();
            assert!(
                close(g.0, want.0, 1e-2) && close(g.1, want.1, 1e-2),
                "{p:?} -> {g:?}, want {want:?}"
            );
        }
    }

    #[test]
    fn degenerate_quad_is_rejected() {
        // All four points collinear -> not convex -> None.
        let img = [(0.1, 0.1), (0.2, 0.2), (0.3, 0.3), (0.4, 0.4)];
        assert!(Homography::from_quad(img, 10.0, 4.0).is_none());
        // Non-positive dimensions rejected.
        let ok = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
        assert!(Homography::from_quad(ok, 0.0, 4.0).is_none());
    }

    #[test]
    fn bowtie_quad_is_rejected() {
        // The same four points wound as a self-intersecting Z (last two swapped)
        // is full-rank but not a simple quad -> must be rejected, not silently
        // accepted into a garbage homography.
        let bowtie = [(0.2, 0.2), (0.8, 0.2), (0.2, 0.8), (0.8, 0.8)];
        assert!(Homography::from_quad(bowtie, 10.0, 4.0).is_none());
    }

    #[test]
    fn behind_horizon_point_is_rejected() {
        // project() must reject points at/behind the horizon (w <= 0) rather than
        // return a mirror-flipped garbage coordinate. Construct a homography whose
        // perspective denominator is w = 1 - 2y, so the horizon is the line
        // y = 0.5: a point below it (w > 0) projects, one above it (w <= 0) is
        // rejected. (With the old `w.abs() < 1e-9` guard the w = -0.4 case would
        // have sailed through and returned garbage.)
        let h = Homography {
            m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -2.0, 1.0],
        };
        assert!(h.project((0.3, 0.7)).is_none()); // w = 1 - 1.4 = -0.4
        assert!(h.project((0.3, 0.3)).is_some()); // w = 1 - 0.6 =  0.4
    }
}
