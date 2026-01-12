//! Camera derivation from player pose.
#![forbid(unsafe_code)]

use rapier3d::math::Vector;
use rapier3d::prelude::Real;

const PITCH_LIMIT: Real = 1.54;

#[derive(Clone, Copy, Debug)]
pub struct CameraPose {
    pub eye: Vector<Real>,
    pub yaw: Real,
    pub pitch: Real,
}

#[derive(Clone, Copy, Debug)]
pub struct PlayerCamera {
    eye_height: Real,
    yaw: Real,
    pitch: Real,
    eye: Vector<Real>,
}

impl PlayerCamera {
    pub fn new(eye_height: Real) -> Self {
        Self {
            eye_height,
            yaw: 0.0,
            pitch: 0.0,
            eye: Vector::zeros(),
        }
    }

    pub fn yaw(&self) -> Real {
        self.yaw
    }

    pub fn pitch(&self) -> Real {
        self.pitch
    }

    pub fn set_look(&mut self, yaw: Real, pitch: Real) {
        self.yaw = yaw;
        self.pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    pub fn apply_look_delta(&mut self, delta: [Real; 2]) {
        self.yaw += delta[0];
        self.pitch = (self.pitch + delta[1]).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    pub fn update_from_origin(&mut self, origin: Vector<Real>) -> CameraPose {
        self.eye = origin + Vector::new(0.0, self.eye_height, 0.0);
        self.pose()
    }

    pub fn pose(&self) -> CameraPose {
        CameraPose {
            eye: self.eye,
            yaw: self.yaw,
            pitch: self.pitch,
        }
    }
}
