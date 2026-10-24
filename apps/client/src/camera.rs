//! Camera and matrix math for the client.
//!
//! Matrices are column-major `[f32; 16]`, the layout WGSL expects. The
//! projection targets WebGPU's `0..1` depth range.

/// Perspective camera looking at a target.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Eye position in world space.
    pub eye: [f32; 3],
    /// Look-at target.
    pub target: [f32; 3],
    /// Up vector.
    pub up: [f32; 3],
    /// Vertical field of view in degrees.
    pub fov_y_deg: f32,
    /// Near plane.
    pub near: f32,
    /// Far plane.
    pub far: f32,
}

impl Camera {
    /// Look from `eye` at `target`.
    pub fn new(eye: [f32; 3], target: [f32; 3]) -> Self {
        Self {
            eye,
            target,
            up: [0.0, 1.0, 0.0],
            fov_y_deg: 55.0,
            near: 0.1,
            far: 400.0,
        }
    }

    /// Column-major view-projection matrix.
    pub fn view_projection(&self, aspect: f32) -> [f32; 16] {
        let projection = perspective(self.fov_y_deg, aspect, self.near, self.far);
        let view = look_at(self.eye, self.target, self.up);
        multiply(projection, view)
    }
}

fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y_deg.to_radians() / 2.0).tan();
    let mut matrix = [0.0f32; 16];
    matrix[0] = f / aspect;
    matrix[5] = f;
    matrix[10] = far / (near - far);
    matrix[11] = -1.0;
    matrix[14] = near * far / (near - far);
    matrix
}

fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let forward = normalize(sub(target, eye));
    let right = normalize(cross(forward, up));
    let true_up = cross(right, forward);
    [
        right[0],
        true_up[0],
        -forward[0],
        0.0,
        right[1],
        true_up[1],
        -forward[1],
        0.0,
        right[2],
        true_up[2],
        -forward[2],
        0.0,
        -dot(right, eye),
        -dot(true_up, eye),
        dot(forward, eye),
        1.0,
    ]
}

fn multiply(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
    let mut result = [0.0f32; 16];
    for column in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for index in 0..4 {
                sum += left[index * 4 + row] * right[column * 4 + index];
            }
            result[column * 4 + row] = sum;
        }
    }
    result
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length = dot(value, value).sqrt();
    if length == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [value[0] / length, value[1] / length, value[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perspective_maps_near_to_zero_and_far_to_one() {
        let matrix = perspective(60.0, 1.0, 0.1, 100.0);
        // Column-major: clip_z = matrix[10] * z + matrix[14], clip_w = -z.
        let near_clip = matrix[10] * -0.1 + matrix[14];
        let near_w = 0.1;
        assert!((near_clip / near_w).abs() < 1e-5);
        let far_clip = matrix[10] * -100.0 + matrix[14];
        let far_w = 100.0;
        assert!((far_clip / far_w - 1.0).abs() < 1e-4);
    }

    #[test]
    fn camera_looking_forward_maps_target_to_negative_z() {
        let camera = Camera::new([0.0, 0.0, 5.0], [0.0, 0.0, 0.0]);
        let matrix = camera.view_projection(1.0);
        // The point at the origin lands on the negative z axis in view space
        // before projection, which the matrix multiplication preserves.
        let x = matrix[0] * 0.0 + matrix[4] * 0.0 + matrix[8] * 0.0 + matrix[12];
        let y = matrix[1] * 0.0 + matrix[5] * 0.0 + matrix[9] * 0.0 + matrix[13];
        let z = matrix[2] * 0.0 + matrix[6] * 0.0 + matrix[10] * 0.0 + matrix[14];
        let w = matrix[3] * 0.0 + matrix[7] * 0.0 + matrix[11] * 0.0 + matrix[15];
        assert!(w > 0.0);
        assert!(x.abs() < 1e-5 && y.abs() < 1e-5);
        assert!(z / w > 0.0 && z / w < 1.0);
    }
}
