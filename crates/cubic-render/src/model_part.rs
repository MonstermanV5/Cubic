//! Bounded Minecraft-style `ModelPart` geometry used by both inventory
//! special models and placed block-entity models.
//!
//! This is an independent representation of the public model-layer format:
//! cubes use the entity-model cube net, parts compose pivot/rotation/scale
//! poses hierarchically, and zero-sized axes remain renderable planes.

use std::f32::consts::PI;

pub(crate) const ALL_FACES: u8 = 0b11_1111;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PartDirection {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl PartDirection {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PartPose {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

impl PartPose {
    pub(crate) const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0; 3],
    };

    pub(crate) const fn offset(x: f32, y: f32, z: f32) -> Self {
        Self {
            translation: [x, y, z],
            ..Self::IDENTITY
        }
    }

    pub(crate) const fn rotation(x: f32, y: f32, z: f32) -> Self {
        Self {
            rotation: [x, y, z],
            ..Self::IDENTITY
        }
    }

    pub(crate) const fn offset_and_rotation(
        x: f32,
        y: f32,
        z: f32,
        x_rot: f32,
        y_rot: f32,
        z_rot: f32,
    ) -> Self {
        Self {
            translation: [x, y, z],
            rotation: [x_rot, y_rot, z_rot],
            scale: [1.0; 3],
        }
    }

    pub(crate) const fn scaled(mut self, scale: f32) -> Self {
        self.scale = [scale; 3];
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CubeDefinition {
    pub origin: [f32; 3],
    pub size: [f32; 3],
    pub texture_offset: [f32; 2],
    pub texture_size: [f32; 2],
    pub deformation: [f32; 3],
    pub mirror: bool,
    pub visible_faces: u8,
}

impl CubeDefinition {
    pub(crate) const fn new(
        origin: [f32; 3],
        size: [f32; 3],
        texture_offset: [f32; 2],
        texture_size: [f32; 2],
    ) -> Self {
        Self {
            origin,
            size,
            texture_offset,
            texture_size,
            deformation: [0.0; 3],
            mirror: false,
            visible_faces: ALL_FACES,
        }
    }

    pub(crate) const fn faces(mut self, faces: u8) -> Self {
        self.visible_faces = faces;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelPartDefinition {
    pub name: &'static str,
    pub pose: PartPose,
    pub visible: bool,
    pub cubes: Vec<CubeDefinition>,
    pub children: Vec<Self>,
}

impl ModelPartDefinition {
    pub(crate) fn root(children: Vec<Self>) -> Self {
        Self {
            name: "root",
            pose: PartPose::IDENTITY,
            visible: true,
            cubes: Vec::new(),
            children,
        }
    }

    pub(crate) fn part(
        name: &'static str,
        pose: PartPose,
        cubes: Vec<CubeDefinition>,
        children: Vec<Self>,
    ) -> Self {
        Self {
            name,
            pose,
            visible: true,
            cubes,
            children,
        }
    }

    pub(crate) fn bake(&self) -> Vec<ModelPartQuad> {
        let mut output = Vec::new();
        self.bake_into(Affine::IDENTITY, &mut output);
        output
    }

    fn bake_into(&self, parent: Affine, output: &mut Vec<ModelPartQuad>) {
        if !self.visible {
            return;
        }
        let transform = parent.multiply(Affine::from_pose(self.pose));
        for cube in &self.cubes {
            bake_cube(*cube, transform, self.name, output);
        }
        for child in &self.children {
            child.bake_into(transform, output);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelPartQuad {
    pub part: &'static str,
    pub direction: PartDirection,
    /// Positions in Minecraft model units (one block is sixteen units).
    pub positions: [[f32; 3]; 4],
    pub uv: [[f32; 2]; 4],
    /// The polygon normal after the authored part hierarchy is applied.
    pub normal: [f32; 3],
    pub determinant_negative: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    matrix: [[f32; 4]; 4],
}

impl Affine {
    const IDENTITY: Self = Self {
        matrix: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    fn from_pose(pose: PartPose) -> Self {
        // ModelPart.translateAndRotate applies translation followed by Z, Y,
        // X rotations and finally the part scale to column vectors.
        let translation = Self::translation(pose.translation);
        let rotation = Self::rotation_z(pose.rotation[2])
            .multiply(Self::rotation_y(pose.rotation[1]))
            .multiply(Self::rotation_x(pose.rotation[0]));
        translation
            .multiply(rotation)
            .multiply(Self::scale(pose.scale))
    }

    fn translation(value: [f32; 3]) -> Self {
        let mut result = Self::IDENTITY;
        for (axis, value) in value.into_iter().enumerate() {
            result.matrix[axis][3] = value;
        }
        result
    }

    fn scale(value: [f32; 3]) -> Self {
        let mut result = Self::IDENTITY;
        for (axis, value) in value.into_iter().enumerate() {
            result.matrix[axis][axis] = value;
        }
        result
    }

    fn rotation_x(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            matrix: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, cos, -sin, 0.0],
                [0.0, sin, cos, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    fn rotation_y(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            matrix: [
                [cos, 0.0, sin, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [-sin, 0.0, cos, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    fn rotation_z(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            matrix: [
                [cos, -sin, 0.0, 0.0],
                [sin, cos, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    fn multiply(self, right: Self) -> Self {
        Self {
            matrix: std::array::from_fn(|row| {
                std::array::from_fn(|column| {
                    (0..4)
                        .map(|index| self.matrix[row][index] * right.matrix[index][column])
                        .sum()
                })
            }),
        }
    }

    fn point(self, point: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|row| {
            self.matrix[row][0] * point[0]
                + self.matrix[row][1] * point[1]
                + self.matrix[row][2] * point[2]
                + self.matrix[row][3]
        })
    }

    fn normal(self, normal: [f32; 3]) -> [f32; 3] {
        // ModelPart poses only use rotation and scale. Apply the inverse
        // transpose so non-uniform and reflected scales preserve the same
        // normal semantics as PoseStack.Pose::transformNormal.
        let m = self.matrix;
        let a = m[0][0];
        let b = m[0][1];
        let c = m[0][2];
        let d = m[1][0];
        let e = m[1][1];
        let f = m[1][2];
        let g = m[2][0];
        let h = m[2][1];
        let i = m[2][2];
        let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        if determinant.abs() <= f32::EPSILON {
            return normal;
        }
        let inverse_transpose = [
            [(e * i - f * h), (f * g - d * i), (d * h - e * g)],
            [(c * h - b * i), (a * i - c * g), (b * g - a * h)],
            [(b * f - c * e), (c * d - a * f), (a * e - b * d)],
        ];
        normalize(std::array::from_fn(|row| {
            inverse_transpose[row][0] * normal[0]
                + inverse_transpose[row][1] * normal[1]
                + inverse_transpose[row][2] * normal[2]
        }))
    }

    fn determinant_negative(self) -> bool {
        let m = self.matrix;
        let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        determinant < 0.0
    }
}

fn bake_cube(
    cube: CubeDefinition,
    transform: Affine,
    part: &'static str,
    output: &mut Vec<ModelPartQuad>,
) {
    let mut min: [f32; 3] = std::array::from_fn(|axis| cube.origin[axis] - cube.deformation[axis]);
    let mut max: [f32; 3] =
        std::array::from_fn(|axis| cube.origin[axis] + cube.size[axis] + cube.deformation[axis]);
    if cube.mirror {
        std::mem::swap(&mut min[0], &mut max[0]);
    }
    let [u, v] = cube.texture_offset;
    let [dx, dy, dz] = cube.size.map(f32::abs);
    let u1 = u + dz;
    let u2 = u1 + dx;
    let u3 = u2 + dx;
    let u4 = u2 + dz;
    let u5 = u4 + dx;
    let v1 = v + dz;
    let v2 = v1 + dy;
    let vertices = [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [max[0], max[1], max[2]],
        [min[0], max[1], max[2]],
    ];
    // Exact ModelPart.Cube polygon construction order for 26.1.2. The four
    // indices are the source Vertex objects passed to Polygon; Polygon then
    // remaps them as (u1,v0), (u0,v0), (u0,v1), (u1,v1).
    let polygons = [
        (PartDirection::Down, [5, 4, 0, 1], [u1, v, u2, v1]),
        (PartDirection::Up, [2, 3, 7, 6], [u2, v1, u3, v]),
        (PartDirection::West, [0, 4, 7, 3], [u, v1, u1, v2]),
        (PartDirection::North, [1, 0, 3, 2], [u1, v1, u2, v2]),
        (PartDirection::East, [5, 1, 2, 6], [u2, v1, u4, v2]),
        (PartDirection::South, [4, 5, 6, 7], [u4, v1, u5, v2]),
    ];
    for (direction, indices, rectangle) in polygons {
        if cube.visible_faces & direction.bit() == 0 {
            continue;
        }
        let mut source_positions = indices.map(|index| vertices[index]);
        let mut uv = polygon_uv(rectangle, cube.texture_size);
        let mut source_normal = direction.normal();
        if cube.mirror {
            source_positions.reverse();
            uv.reverse();
            if matches!(direction, PartDirection::West | PartDirection::East) {
                source_normal[0] = -source_normal[0];
            }
        }
        let mut positions = source_positions.map(|point| transform.point(point));
        // Vanilla leaves reflection to the render pipeline. Cubic emits
        // ordinary front-face triangles, so compensate only for a hierarchy
        // reflection after reproducing the source Polygon exactly.
        if transform.determinant_negative() {
            positions.swap(1, 3);
            uv.swap(1, 3);
        }
        output.push(ModelPartQuad {
            part,
            direction,
            positions,
            uv,
            normal: transform.normal(source_normal),
            determinant_negative: transform.determinant_negative(),
        });
    }
}

fn polygon_uv(rectangle: [f32; 4], texture_size: [f32; 2]) -> [[f32; 2]; 4] {
    let [u0, v0, u1, v1] = rectangle;
    [
        [u1 / texture_size[0], v0 / texture_size[1]],
        [u0 / texture_size[0], v0 / texture_size[1]],
        [u0 / texture_size[0], v1 / texture_size[1]],
        [u1 / texture_size[0], v1 / texture_size[1]],
    ]
}

impl PartDirection {
    const fn normal(self) -> [f32; 3] {
        match self {
            Self::Down => [0.0, -1.0, 0.0],
            Self::Up => [0.0, 1.0, 0.0],
            Self::North => [0.0, 0.0, -1.0],
            Self::South => [0.0, 0.0, 1.0],
            Self::West => [-1.0, 0.0, 0.0],
            Self::East => [1.0, 0.0, 0.0],
        }
    }
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length = value.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
    if length <= f32::EPSILON {
        value
    } else {
        value.map(|axis| axis / length)
    }
}

pub(crate) const HALF_PI: f32 = PI * 0.5;

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: [f32; 3], right: [f32; 3]) -> bool {
        left.into_iter()
            .zip(right)
            .all(|(left, right)| (left - right).abs() < 1.0e-5)
    }

    #[test]
    fn parent_child_pose_composes_translation_and_rotation() {
        let model = ModelPartDefinition::root(vec![ModelPartDefinition::part(
            "parent",
            PartPose::offset_and_rotation(4.0, 0.0, 0.0, 0.0, 0.0, HALF_PI),
            vec![],
            vec![ModelPartDefinition::part(
                "child",
                PartPose::offset(2.0, 0.0, 0.0),
                vec![CubeDefinition::new([0.0; 3], [1.0; 3], [0.0; 2], [16.0; 2])],
                vec![],
            )],
        )]);
        let quad = &model.bake()[0];
        assert!(
            quad.positions
                .iter()
                .any(|point| close(*point, [4.0, 2.0, 1.0]))
        );
    }

    #[test]
    fn negative_scale_reverses_winding_but_preserves_uv_correspondence() {
        let model = ModelPartDefinition::part(
            "reflected",
            PartPose {
                scale: [1.0, -1.0, -1.0],
                ..PartPose::IDENTITY
            },
            vec![CubeDefinition::new([0.0; 3], [2.0; 3], [0.0; 2], [16.0; 2])],
            vec![],
        );
        let quads = model.bake();
        assert!(quads.iter().all(|quad| !quad.determinant_negative));
        assert_eq!(quads[0].uv[0], [4.0 / 16.0, 0.0]);
    }

    #[test]
    fn odd_reflection_reports_negative_determinant_and_keeps_front_winding() {
        let plain = ModelPartDefinition::part(
            "plain",
            PartPose::IDENTITY,
            vec![CubeDefinition::new([0.0; 3], [2.0; 3], [0.0; 2], [16.0; 2])],
            vec![],
        )
        .bake();
        let reflected = ModelPartDefinition::part(
            "reflected",
            PartPose {
                scale: [-1.0, 1.0, 1.0],
                ..PartPose::IDENTITY
            },
            vec![CubeDefinition::new([0.0; 3], [2.0; 3], [0.0; 2], [16.0; 2])],
            vec![],
        )
        .bake();
        assert!(reflected.iter().all(|quad| quad.determinant_negative));
        for uv in plain[0].uv {
            assert!(reflected[0].uv.contains(&uv));
        }
    }

    #[test]
    fn zero_thickness_cube_retains_its_one_sided_plane() {
        let north_only = 1 << PartDirection::North as u8;
        let model = ModelPartDefinition::part(
            "plane",
            PartPose::IDENTITY,
            vec![
                CubeDefinition::new([0.0; 3], [14.0, 16.0, 0.0], [1.0, 0.0], [16.0; 2])
                    .faces(north_only),
            ],
            vec![],
        );
        let quads = model.bake();
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].direction, PartDirection::North);
    }

    #[test]
    fn entity_cube_net_maps_smaller_v_to_smaller_y_on_vertical_faces() {
        let model = ModelPartDefinition::part(
            "chest_side",
            PartPose::IDENTITY,
            vec![CubeDefinition::new(
                [1.0, 0.0, 1.0],
                [14.0, 10.0, 14.0],
                [0.0, 19.0],
                [64.0; 2],
            )],
            vec![],
        );
        let north = model
            .bake()
            .into_iter()
            .find(|quad| quad.direction == PartDirection::North)
            .unwrap();
        let top_v = north
            .positions
            .iter()
            .zip(north.uv)
            .filter(|(point, _)| point[1] == 0.0)
            .map(|(_, uv)| uv[1])
            .fold(f32::INFINITY, f32::min);
        let bottom_v = north
            .positions
            .iter()
            .zip(north.uv)
            .filter(|(point, _)| point[1] == 10.0)
            .map(|(_, uv)| uv[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(top_v < bottom_v);
    }

    #[test]
    fn cube_polygons_match_the_vanilla_directional_vertex_and_uv_oracle() {
        let quads = ModelPartDefinition::part(
            "oracle",
            PartPose::IDENTITY,
            vec![CubeDefinition::new(
                [1.0, 2.0, 3.0],
                [4.0, 5.0, 6.0],
                [10.0, 20.0],
                [64.0, 64.0],
            )],
            vec![],
        )
        .bake();
        let p = [
            [1.0, 2.0, 3.0],
            [5.0, 2.0, 3.0],
            [5.0, 7.0, 3.0],
            [1.0, 7.0, 3.0],
            [1.0, 2.0, 9.0],
            [5.0, 2.0, 9.0],
            [5.0, 7.0, 9.0],
            [1.0, 7.0, 9.0],
        ];
        let expected = [
            (PartDirection::Down, [5, 4, 0, 1], [16.0, 20.0, 20.0, 26.0]),
            (PartDirection::Up, [2, 3, 7, 6], [20.0, 26.0, 24.0, 20.0]),
            (PartDirection::West, [0, 4, 7, 3], [10.0, 26.0, 16.0, 31.0]),
            (PartDirection::North, [1, 0, 3, 2], [16.0, 26.0, 20.0, 31.0]),
            (PartDirection::East, [5, 1, 2, 6], [20.0, 26.0, 26.0, 31.0]),
            (PartDirection::South, [4, 5, 6, 7], [26.0, 26.0, 30.0, 31.0]),
        ];
        for (quad, (direction, indices, rectangle)) in quads.iter().zip(expected) {
            assert_eq!(quad.direction, direction);
            assert_eq!(quad.positions, indices.map(|index| p[index]));
            assert_eq!(quad.uv, polygon_uv(rectangle, [64.0, 64.0]));
            assert_eq!(quad.normal, direction.normal());
        }
    }

    #[test]
    fn mirrored_cube_reverses_vertices_and_only_mirrors_x_facing_normals() {
        let mut cube = CubeDefinition::new([1.0; 3], [2.0, 3.0, 4.0], [0.0; 2], [32.0; 2]);
        let plain =
            ModelPartDefinition::part("plain", PartPose::IDENTITY, vec![cube], vec![]).bake();
        cube.mirror = true;
        let mirrored =
            ModelPartDefinition::part("mirror", PartPose::IDENTITY, vec![cube], vec![]).bake();
        for (plain, mirrored) in plain.iter().zip(mirrored) {
            let expected_normal =
                if matches!(plain.direction, PartDirection::West | PartDirection::East) {
                    plain.normal.map(|axis| -axis)
                } else {
                    plain.normal
                };
            assert_eq!(mirrored.normal, expected_normal);
            assert_eq!(
                mirrored.uv,
                [plain.uv[3], plain.uv[2], plain.uv[1], plain.uv[0]]
            );
        }
    }

    #[test]
    fn zero_height_cube_preserves_directional_polygon_construction() {
        let up_only = 1 << PartDirection::Up as u8;
        let quads = ModelPartDefinition::part(
            "flat",
            PartPose::IDENTITY,
            vec![
                CubeDefinition::new([0.0; 3], [8.0, 0.0, 8.0], [0.0; 2], [32.0; 2]).faces(up_only),
            ],
            vec![],
        )
        .bake();
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].direction, PartDirection::Up);
        assert!(quads[0].positions.iter().all(|point| point[1] == 0.0));
        assert_eq!(quads[0].normal, [0.0, 1.0, 0.0]);
    }
}
