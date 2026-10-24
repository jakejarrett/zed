//! What changed between two frames, so a renderer that keeps the last frame
//! can draw only that part again.
//!
//! Each primitive is hashed with everything but its draw order (inserting one
//! primitive renumbers every later one without changing how they look), and
//! the two frames' hashes are compared as multisets. The damage is the union
//! of the bounds of the primitives only one frame has: where something
//! appeared, disappeared or moved.

use collections::FxHasher;
use gpui::{
    Bounds, DevicePixels, MonochromeSprite, Path, PolychromeSprite, Quad, ScaledPixels, Scene,
    Shadow, Size, SubpixelSprite, TransformationMatrix, Underline, point, size,
};
use std::hash::Hasher;

// Hashing skips the leading `order` field of each primitive.
const _: () = {
    assert!(std::mem::offset_of!(Quad, order) == 0);
    assert!(std::mem::offset_of!(Shadow, order) == 0);
    assert!(std::mem::offset_of!(Underline, order) == 0);
    assert!(std::mem::offset_of!(MonochromeSprite, order) == 0);
    assert!(std::mem::offset_of!(SubpixelSprite, order) == 0);
    assert!(std::mem::offset_of!(PolychromeSprite, order) == 0);
};

/// Past this share of the window, drawing it all costs no more than drawing
/// the damage, and skips the bookkeeping of a partial frame.
const FULL_SHARE: f32 = 0.6;

/// What one frame drew, reduced to what comparing frames needs.
pub(crate) struct FrameDigest {
    size: Size<DevicePixels>,
    /// Each primitive's hash and the area it can touch, sorted by hash.
    entries: Vec<(u64, Bounds<ScaledPixels>)>,
    /// A surface (video) changes without its primitive changing.
    has_surfaces: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Damage {
    /// Nothing looks different.
    None,
    /// Draw again inside these bounds, in device pixels.
    Region(Bounds<ScaledPixels>),
    /// Draw everything.
    Full,
}

impl FrameDigest {
    pub(crate) fn of(scene: &Scene, size: Size<DevicePixels>) -> Self {
        let mut entries = Vec::with_capacity(
            scene.quads.len()
                + scene.shadows.len()
                + scene.paths.len()
                + scene.underlines.len()
                + scene.monochrome_sprites.len()
                + scene.subpixel_sprites.len()
                + scene.polychrome_sprites.len(),
        );
        for quad in &scene.quads {
            entries.push((
                hash_after_order(0, quad),
                quad.bounds.intersect(&quad.content_mask.bounds),
            ));
        }
        for shadow in &scene.shadows {
            // The shader draws three blur radii past the shadow's own bounds.
            let reach = shadow.bounds.dilate(shadow.blur_radius * 3.0);
            entries.push((
                hash_after_order(1, shadow),
                reach.intersect(&shadow.content_mask.bounds),
            ));
        }
        for underline in &scene.underlines {
            let reach = underline.bounds.dilate(underline.thickness);
            entries.push((
                hash_after_order(2, underline),
                reach.intersect(&underline.content_mask.bounds),
            ));
        }
        for sprite in &scene.monochrome_sprites {
            entries.push((
                hash_after_order(3, sprite),
                sprite_reach(sprite.bounds, &sprite.transformation, sprite.content_mask.bounds),
            ));
        }
        for sprite in &scene.subpixel_sprites {
            entries.push((
                hash_after_order(4, sprite),
                sprite_reach(sprite.bounds, &sprite.transformation, sprite.content_mask.bounds),
            ));
        }
        for sprite in &scene.polychrome_sprites {
            entries.push((
                hash_after_order(5, sprite),
                sprite.bounds.intersect(&sprite.content_mask.bounds),
            ));
        }
        for path in &scene.paths {
            entries.push((hash_path(path), path.clipped_bounds()));
        }
        entries.sort_unstable_by_key(|(hash, _)| *hash);
        Self {
            size,
            entries,
            has_surfaces: !scene.surfaces.is_empty(),
        }
    }

    /// What has to be drawn again to turn `previous` into this frame.
    pub(crate) fn damage_since(&self, previous: Option<&FrameDigest>) -> Damage {
        let Some(previous) = previous else {
            return Damage::Full;
        };
        if previous.size != self.size || self.has_surfaces || previous.has_surfaces {
            return Damage::Full;
        }
        let mut damage: Option<Bounds<ScaledPixels>> = None;
        let mut add = |bounds: Bounds<ScaledPixels>| {
            if bounds.size.width.0 <= 0.0 || bounds.size.height.0 <= 0.0 {
                return;
            }
            damage = Some(match damage {
                Some(damage) => damage.union(&bounds),
                None => bounds,
            });
        };
        // Both are sorted by hash: walk them together, letting equal hashes
        // cancel one for one.
        let (mut old, mut new) = (previous.entries.iter().peekable(), self.entries.iter().peekable());
        loop {
            match (old.peek(), new.peek()) {
                (Some((a, a_bounds)), Some((b, b_bounds))) => {
                    if a == b {
                        old.next();
                        new.next();
                    } else if a < b {
                        add(*a_bounds);
                        old.next();
                    } else {
                        add(*b_bounds);
                        new.next();
                    }
                }
                (Some((_, bounds)), None) => {
                    add(*bounds);
                    old.next();
                }
                (None, Some((_, bounds))) => {
                    add(*bounds);
                    new.next();
                }
                (None, None) => break,
            }
        }
        let viewport = Bounds {
            origin: point(ScaledPixels(0.0), ScaledPixels(0.0)),
            size: size(
                ScaledPixels(self.size.width.0 as f32),
                ScaledPixels(self.size.height.0 as f32),
            ),
        };
        let Some(damage) = damage.map(|damage| damage.intersect(&viewport)) else {
            return Damage::None;
        };
        if damage.size.width.0 <= 0.0 || damage.size.height.0 <= 0.0 {
            return Damage::None;
        }
        let share = (damage.size.width.0 * damage.size.height.0)
            / (viewport.size.width.0 * viewport.size.height.0).max(1.0);
        if share > FULL_SHARE {
            Damage::Full
        } else {
            Damage::Region(damage)
        }
    }
}

/// Where a glyph or icon can land: its bounds, or its whole clip when it is
/// transformed (rotated text), whose bounds are before the transform.
fn sprite_reach(
    bounds: Bounds<ScaledPixels>,
    transformation: &TransformationMatrix,
    mask: Bounds<ScaledPixels>,
) -> Bounds<ScaledPixels> {
    if *transformation == TransformationMatrix::unit() {
        bounds.intersect(&mask)
    } else {
        mask
    }
}

fn hash_after_order<T: Copy>(kind: u8, primitive: &T) -> u64 {
    // Every field after `order` (checked above); primitives are `repr(C)` with
    // explicit padding, as uploaded to the GPU as they are.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (primitive as *const T as *const u8).add(size_of::<u32>()),
            size_of::<T>() - size_of::<u32>(),
        )
    };
    let mut hasher = FxHasher::default();
    hasher.write_u8(kind);
    hasher.write(bytes);
    hasher.finish()
}

fn hash_path(path: &Path<ScaledPixels>) -> u64 {
    let mut hasher = FxHasher::default();
    hasher.write_u8(6);
    hasher.write(bytes_of(&path.bounds));
    hasher.write(bytes_of(&path.content_mask));
    hasher.write(bytes_of(&path.color));
    let vertices = unsafe {
        std::slice::from_raw_parts(
            path.vertices.as_ptr() as *const u8,
            std::mem::size_of_val(path.vertices.as_slice()),
        )
    };
    hasher.write(vertices);
    hasher.finish()
}

fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ContentMask, Hsla};

    fn quad(x: f32, y: f32, order: u32) -> Quad {
        let bounds = Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(10.0), ScaledPixels(10.0)),
        };
        Quad {
            order,
            bounds,
            content_mask: ContentMask {
                bounds: Bounds {
                    origin: point(ScaledPixels(0.0), ScaledPixels(0.0)),
                    size: size(ScaledPixels(1000.0), ScaledPixels(1000.0)),
                },
            },
            background: Hsla::red().into(),
            ..Default::default()
        }
    }

    fn scene(quads: &[Quad]) -> Scene {
        let mut scene = Scene::default();
        scene.quads = quads.to_vec();
        scene
    }

    const SIZE: Size<DevicePixels> = Size {
        width: DevicePixels(1000),
        height: DevicePixels(1000),
    };

    #[test]
    fn the_first_frame_is_drawn_whole() {
        let frame = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0)]), SIZE);
        assert_eq!(frame.damage_since(None), Damage::Full);
    }

    #[test]
    fn an_unchanged_frame_has_no_damage() {
        let before = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0), quad(50.0, 0.0, 1)]), SIZE);
        let after = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0), quad(50.0, 0.0, 1)]), SIZE);
        assert_eq!(after.damage_since(Some(&before)), Damage::None);
    }

    #[test]
    fn renumbered_primitives_are_not_damage() {
        let before = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0), quad(50.0, 0.0, 1)]), SIZE);
        let after = FrameDigest::of(&scene(&[quad(0.0, 0.0, 7), quad(50.0, 0.0, 9)]), SIZE);
        assert_eq!(after.damage_since(Some(&before)), Damage::None);
    }

    #[test]
    fn a_moved_quad_damages_where_it_was_and_where_it_is() {
        let before = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0), quad(100.0, 100.0, 1)]), SIZE);
        let after = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0), quad(120.0, 100.0, 1)]), SIZE);
        assert_eq!(
            after.damage_since(Some(&before)),
            Damage::Region(Bounds {
                origin: point(ScaledPixels(100.0), ScaledPixels(100.0)),
                size: size(ScaledPixels(30.0), ScaledPixels(10.0)),
            })
        );
    }

    #[test]
    fn a_new_shadow_damages_its_blur_too() {
        let before = FrameDigest::of(&scene(&[]), SIZE);
        let mut with_shadow = scene(&[]);
        let mask = quad(0.0, 0.0, 0).content_mask;
        with_shadow.shadows.push(Shadow {
            order: 0,
            blur_radius: ScaledPixels(2.0),
            bounds: Bounds {
                origin: point(ScaledPixels(100.0), ScaledPixels(100.0)),
                size: size(ScaledPixels(10.0), ScaledPixels(10.0)),
            },
            corner_radii: Default::default(),
            content_mask: mask,
            color: Hsla::black(),
            element_bounds: Default::default(),
            element_corner_radii: Default::default(),
            inset: 0,
            pad: 0,
        });
        let after = FrameDigest::of(&with_shadow, SIZE);
        assert_eq!(
            after.damage_since(Some(&before)),
            Damage::Region(Bounds {
                origin: point(ScaledPixels(94.0), ScaledPixels(94.0)),
                size: size(ScaledPixels(22.0), ScaledPixels(22.0)),
            })
        );
    }

    #[test]
    fn a_resize_is_drawn_whole() {
        let before = FrameDigest::of(&scene(&[quad(0.0, 0.0, 0)]), SIZE);
        let after = FrameDigest::of(
            &scene(&[quad(0.0, 0.0, 0)]),
            Size {
                width: DevicePixels(800),
                height: DevicePixels(1000),
            },
        );
        assert_eq!(after.damage_since(Some(&before)), Damage::Full);
    }

    #[test]
    fn damage_over_most_of_the_window_is_drawn_whole() {
        let mut big = quad(0.0, 0.0, 0);
        big.bounds.size = size(ScaledPixels(900.0), ScaledPixels(900.0));
        let before = FrameDigest::of(&scene(&[]), SIZE);
        let after = FrameDigest::of(&scene(&[big]), SIZE);
        assert_eq!(after.damage_since(Some(&before)), Damage::Full);
    }
}
