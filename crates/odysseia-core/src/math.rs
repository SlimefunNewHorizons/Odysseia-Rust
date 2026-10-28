use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vector3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vector3D {
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub fn distance_squared(&self, other: &Vector3D) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        dx * dx + dy * dy + dz * dz
    }

    pub fn distance(&self, other: &Vector3D) -> f64 {
        self.distance_squared(other).sqrt()
    }

    pub fn normalize(&self) -> Self {
        let len = (self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        if len == 0.0 {
            Self::new(0.0, 0.0, 0.0)
        } else {
            Self::new(self.x / len, self.y / len, self.z / len)
        }
    }
}

/// Calcula si un jugador está dentro de la onda expansiva o aura de un Boss.
pub fn is_in_boss_aura(boss_pos: Vector3D, player_pos: Vector3D, aura_radius: f64) -> bool {
    boss_pos.distance_squared(&player_pos) <= (aura_radius * aura_radius)
}

/// Batch aura test: `points` is `[x0,y0,z0,x1,y1,z1,...]`; writes 1/0 per point
/// into `out` and returns the number of hits. One call per tick instead of one per player.
pub fn aura_hits(boss: Vector3D, radius: f64, points: &[f64], out: &mut [u8]) -> usize {
    let r2 = radius * radius;
    let mut hits = 0;
    for (p, o) in points.chunks_exact(3).zip(out.iter_mut()) {
        let (dx, dy, dz) = (p[0] - boss.x, p[1] - boss.y, p[2] - boss.z);
        let inside = dx * dx + dy * dy + dz * dz <= r2;
        *o = inside as u8;
        hits += inside as usize;
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_matches_single() {
        let boss = Vector3D::new(0.0, 64.0, 0.0);
        let pts = [1.0, 64.0, 1.0, 50.0, 64.0, 0.0, 0.0, 64.0, 5.0];
        let mut out = [9u8; 3];
        assert_eq!(aura_hits(boss, 5.0, &pts, &mut out), 2);
        for (i, p) in pts.chunks(3).enumerate() {
            assert_eq!(out[i] == 1, is_in_boss_aura(boss, Vector3D::new(p[0], p[1], p[2]), 5.0));
        }
    }
}
