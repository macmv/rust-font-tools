use otspec::{DeserializationError, Deserializer, ReaderContext};

/// Structures for handling components within a composite glyph
mod component;
/// Structures for handling simple glyph descriptions
mod glyph;
/// A representation of a contour point
mod point;

pub use component::{Component, ComponentFlags};
pub use glyph::Glyph;
pub use point::Point;

/// The glyf table
#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq)]
pub struct glyf {
    /// A list of glyph objects in the font
    pub glyphs: Vec<Glyph>,
}

/// Deserialize the font from a binary buffer.
///
/// loca_offsets must be obtained from the `loca` table.
pub fn from_bytes(c: &[u8], loca_offsets: Vec<Option<u32>>) -> Result<glyf, DeserializationError> {
    from_rc(&mut ReaderContext::new(c.to_vec()), loca_offsets)
}

pub fn from_rc(
    c: &mut ReaderContext,
    loca_offsets: Vec<Option<u32>>,
) -> Result<glyf, DeserializationError> {
    let mut res = glyf { glyphs: Vec::new() };
    for item in loca_offsets {
        match item {
            None => res.glyphs.push(Glyph {
                contours: vec![],
                components: vec![],
                overlap: false,
                xMax: 0,
                xMin: 0,
                yMax: 0,
                yMin: 0,
                instructions: vec![],
            }),
            Some(item) => {
                let old = c.ptr;
                c.ptr = item as usize;
                let glyph: Glyph = c.de()?;
                res.glyphs.push(glyph);
                c.ptr = old;
            }
        }
    }
    Ok(res)
}

impl glyf {
    /// Given a `Glyph` object, return all components used by this glyph,
    /// including recursively descending into nested components and positioning
    /// them accordingly. Should be called with `depth=0`.
    pub fn flat_components(&self, g: &Glyph, depth: u32) -> Vec<Component> {
        let mut new_components = vec![];
        if depth > 64 {
            log::warn!(
                "Extremely deeply nested component in glyph {:?}. Possible loop?",
                g
            );
            return new_components;
        }
        for comp in &g.components {
            let component_glyph = &self.glyphs[comp.glyph_index as usize];
            if component_glyph.has_components() {
                let mut flattened = self.flat_components(&component_glyph, depth + 1);
                for f in flattened.iter_mut() {
                    f.transformation = comp.transformation * f.transformation;
                    // This may be the wrong way around...
                }
                new_components.extend(flattened);
            } else {
                new_components.push(comp.clone());
            }
        }
        new_components
    }

    /// Flattens all components in this table, replacing nested components with
    /// a single level of correctly positioned components.
    pub fn flatten_components(&mut self) {
        let mut needs_flattening = vec![];
        for (id, g) in self.glyphs.iter().enumerate() {
            if !g.has_components() {
                continue;
            }
            let flat = self.flat_components(g, 0);
            if g.components != flat {
                needs_flattening.push((id, flat));
            }
        }
        for (id, comp) in needs_flattening {
            self.glyphs[id].components = comp;
        }
    }
    /// Recalculate the bounds of all glyphs within the table.
    /// *Note* that this flattens nested components.
    pub fn recalc_bounds(&mut self) {
        self.flatten_components();
        // First do simple glyphs
        for g in self.glyphs.iter_mut() {
            if !g.has_components() {
                let (x_pts, y_pts): (Vec<i16>, Vec<i16>) =
                    g.contours.iter().flatten().map(|pt| (pt.x, pt.y)).unzip();
                g.xMin = *x_pts.iter().min().unwrap_or(&0);
                g.xMax = *x_pts.iter().max().unwrap_or(&0);
                g.yMin = *y_pts.iter().min().unwrap_or(&0);
                g.yMax = *y_pts.iter().max().unwrap_or(&0);
            }
        }

        // Gather boxes
        let boxes: Vec<kurbo::Rect> = self.glyphs.iter().map(|x| x.bounds_rect()).collect();

        // Now do component
        for g in self.glyphs.iter_mut() {
            let mut done = false;
            if !g.has_components() {
                continue;
            }
            for comp in &g.components {
                if comp.flags.contains(ComponentFlags::USE_MY_METRICS) {
                    let component_bounds = boxes[comp.glyph_index as usize];
                    g.set_bounds_rect(component_bounds);
                    done = true;
                    break;
                }
            }
            if !done {
                let newbounds = g
                    .components
                    .iter()
                    .map({
                        |comp| {
                            let component_bounds = boxes[comp.glyph_index as usize];
                            comp.transformation.transform_rect_bbox(component_bounds)
                        }
                    })
                    .reduce(|a, b| a.union(b))
                    .unwrap();
                g.set_bounds_rect(newbounds);
            }
        }
    }
    /// Gathers statistics to be used in the `maxp` table, returning a tuple of
    /// `(num_glyphs, max_points, max_contours, max_composite_points,
    /// max_composite_contours, max_component_elements, max_component_depth)`
    pub fn maxp_statistics(&self) -> (u16, u16, u16, u16, u16, u16, u16) {
        let num_glyphs = self.glyphs.len() as u16;
        let max_points = self
            .glyphs
            .iter()
            .map(|x| x.contours.iter().map(|c| c.len()).sum())
            .max()
            .unwrap_or(0) as u16;

        let max_contours = self
            .glyphs
            .iter()
            .map(|x| x.contours.len())
            .max()
            .unwrap_or(0) as u16;
        let max_composite_points = 0;
        let max_composite_contours = 0;
        let max_component_elements = self
            .glyphs
            .iter()
            .map(|x| x.components.len())
            .max()
            .unwrap_or(0) as u16;
        let max_component_depth = 1; // XXX
        (
            num_glyphs,
            max_points,
            max_contours,
            max_composite_points,
            max_composite_contours,
            max_component_elements,
            max_component_depth,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::font;
    use crate::glyf;
    use crate::glyf::ComponentFlags;
    use crate::glyf::Point;

    #[test]
    fn glyf_de() {
        let binary_glyf = vec![
            0x00, 0x02, // Two contours
            0x00, 0x14, // xMin
            0x00, 0x00, // yMin
            0x02, 0x37, // xMax
            0x01, 0x22, // yMax
            0x00, 0x02, // end of first
            0x00, 0x0e, // end of second
            0x00, 0x00, // No instructions
            /* First contour flags */
            0x33, // pt0: Oncurve. X is short. Y is repeated.
            0x33, // pt1: Oncurve. X is short. Y is repeated.
            0x27, // pt2: Oncurve. X is short + negative. Y is short + positive.
            /* Second contour flags */
            0x24, // pt3: Offcurve. Y is short + positive
            0x36, // pt4:
            0x33, // pt5: Oncurve. X is short. Y is repeated.
            0x32, // pt6: Offcurve. X is short. Y is repeated.
            0x16, // pt7:
            0x15, // pt8:
            0x14, // pt9:
            0x06, // pt10: On curve, x and y short
            0x23, // pt11:
            0x22, // pt12:
            0x26, // pt13:
            0x35, // pt14:
            /* Point 0 */
            0x14, // X = 20
            /* Point 1 */
            0xc8, // X += 200
            /* Point 2 */
            0x78, // X -= 120
            0x01, // ???
            0x1e, 0x36, 0x25, 0x25, 0x35, 0x35, 0x25, 0x25, 0x36, 0xc8, 0x25, 0x35, 0x35, 0x25,
            0x25, 0x36, 0x36, 0x25,
        ];
        let deserialized = otspec::de::from_bytes::<glyf::Glyph>(&binary_glyf).unwrap();
        #[rustfmt::skip]
        let glyph = glyf::Glyph {
            xMin: 20, xMax: 567, yMin: 0, yMax: 290,
            contours: vec![
                vec![
                    Point {x: 20, y: 0, on_curve: true, },
                    Point {x: 220, y: 0, on_curve: true, },
                    Point {x: 100, y: 200, on_curve: true, },
                ],
                vec![
                    Point {x: 386, y: 237, on_curve: false, },
                    Point {x: 440, y: 290, on_curve: false, },
                    Point {x: 477, y: 290, on_curve: true, },
                    Point {x: 514, y: 290, on_curve: false, },
                    Point {x: 567, y: 237, on_curve: false, },
                    Point {x: 567, y: 200, on_curve: true, },
                    Point {x: 567, y: 163, on_curve: false, },
                    Point {x: 514, y: 109, on_curve: false, },
                    Point {x: 477, y: 109, on_curve: true, },
                    Point {x: 440, y: 109, on_curve: false, },
                    Point {x: 386, y: 163, on_curve: false, },
                    Point {x: 386, y: 200, on_curve: true, },
                ],
            ],
            instructions: vec![],
            components: vec![],
            overlap: false,
        };
        assert_eq!(deserialized, glyph);
        let serialized = otspec::ser::to_bytes(&glyph).unwrap();
        // println!("Got:      {:02x?}", serialized);
        // println!("Expected: {:02x?}", binary_glyf);
        assert_eq!(serialized, binary_glyf);
    }

    #[test]
    fn test_glyf_de() {
        let binary_font = vec![
            0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x80, 0x00, 0x03, 0x00, 0x20, 0x4f, 0x53,
            0x2f, 0x32, 0x47, 0x36, 0x45, 0x90, 0x00, 0x00, 0x01, 0x28, 0x00, 0x00, 0x00, 0x60,
            0x63, 0x6d, 0x61, 0x70, 0x01, 0x5c, 0x04, 0x51, 0x00, 0x00, 0x01, 0xa8, 0x00, 0x00,
            0x00, 0x64, 0x67, 0x6c, 0x79, 0x66, 0x01, 0x73, 0xbf, 0xf8, 0x00, 0x00, 0x02, 0x20,
            0x00, 0x00, 0x02, 0x1e, 0x68, 0x65, 0x61, 0x64, 0x1a, 0x46, 0x65, 0x4f, 0x00, 0x00,
            0x00, 0xac, 0x00, 0x00, 0x00, 0x36, 0x68, 0x68, 0x65, 0x61, 0x05, 0x85, 0x01, 0xc2,
            0x00, 0x00, 0x00, 0xe4, 0x00, 0x00, 0x00, 0x24, 0x68, 0x6d, 0x74, 0x78, 0x10, 0xf6,
            0xff, 0xda, 0x00, 0x00, 0x01, 0x88, 0x00, 0x00, 0x00, 0x20, 0x6c, 0x6f, 0x63, 0x61,
            0x02, 0x55, 0x01, 0xd6, 0x00, 0x00, 0x02, 0x0c, 0x00, 0x00, 0x00, 0x12, 0x6d, 0x61,
            0x78, 0x70, 0x00, 0x12, 0x00, 0x47, 0x00, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x20,
            0x6e, 0x61, 0x6d, 0x65, 0xff, 0x72, 0x0d, 0x88, 0x00, 0x00, 0x04, 0x40, 0x00, 0x00,
            0x00, 0xb4, 0x70, 0x6f, 0x73, 0x74, 0x16, 0xf9, 0xc6, 0xb7, 0x00, 0x00, 0x04, 0xf4,
            0x00, 0x00, 0x00, 0x48, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x7e, 0x62,
            0x06, 0x11, 0x5f, 0x0f, 0x3c, 0xf5, 0x00, 0x03, 0x03, 0xe8, 0x00, 0x00, 0x00, 0x00,
            0xdc, 0x27, 0x59, 0x19, 0x00, 0x00, 0x00, 0x00, 0xdc, 0xa5, 0xc8, 0x08, 0xff, 0x73,
            0xff, 0xb4, 0x02, 0xef, 0x03, 0x93, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x02, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x03, 0x20, 0xff, 0x38, 0x00, 0x00,
            0x02, 0xf4, 0xff, 0x73, 0xff, 0x8d, 0x02, 0xef, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x08, 0x00, 0x34, 0x00, 0x03, 0x00, 0x10, 0x00, 0x04, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x01, 0x00, 0x03, 0x02, 0x1f, 0x01, 0x90, 0x00, 0x05, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x3f, 0x3f, 0x3f, 0x3f, 0x00, 0x00, 0x00, 0x20, 0x03, 0x01,
            0x03, 0x20, 0xff, 0x38, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0xf4, 0x02, 0xbc, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00,
            0x02, 0xf4, 0x00, 0x05, 0x02, 0xf4, 0x00, 0x05, 0x02, 0x98, 0x00, 0x1e, 0x02, 0xf4,
            0x00, 0x05, 0x00, 0xc8, 0x00, 0x00, 0x02, 0x58, 0x00, 0x1d, 0x02, 0x58, 0x00, 0x1d,
            0x00, 0x0a, 0xff, 0x73, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00,
            0x00, 0x14, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x14, 0x00, 0x04, 0x00, 0x50,
            0x00, 0x00, 0x00, 0x10, 0x00, 0x10, 0x00, 0x03, 0x00, 0x00, 0x00, 0x20, 0x00, 0x24,
            0x00, 0x41, 0x00, 0x4f, 0x00, 0x56, 0x00, 0xc1, 0x03, 0x01, 0xff, 0xff, 0x00, 0x00,
            0x00, 0x20, 0x00, 0x24, 0x00, 0x41, 0x00, 0x4f, 0x00, 0x56, 0x00, 0xc1, 0x03, 0x01,
            0xff, 0xff, 0xff, 0xe4, 0xff, 0xe1, 0xff, 0xbf, 0xff, 0xb3, 0xff, 0xad, 0xff, 0x40,
            0xfd, 0x06, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e, 0x00, 0x2a, 0x00, 0x51,
            0x00, 0x66, 0x00, 0x66, 0x00, 0xb6, 0x01, 0x01, 0x01, 0x0f, 0x00, 0x00, 0x00, 0x03,
            0x00, 0x05, 0x00, 0x00, 0x02, 0xef, 0x02, 0xbc, 0x00, 0x03, 0x00, 0x07, 0x00, 0x0b,
            0x00, 0x00, 0x01, 0x01, 0x33, 0x01, 0x23, 0x01, 0x33, 0x01, 0x13, 0x35, 0x21, 0x15,
            0x01, 0x43, 0x01, 0x3e, 0x6e, 0xfe, 0xc2, 0x6e, 0xfe, 0xc2, 0x6e, 0x01, 0x3e, 0x86,
            0xfe, 0x61, 0x02, 0xbc, 0xfd, 0x44, 0x02, 0xbc, 0xfd, 0x44, 0x02, 0xbc, 0xfe, 0x10,
            0x50, 0x50, 0xff, 0xff, 0x00, 0x05, 0x00, 0x00, 0x02, 0xef, 0x03, 0x93, 0x00, 0x26,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x07, 0x01, 0x92, 0x00, 0x82, 0x00, 0x02,
            0x00, 0x1e, 0xff, 0xf6, 0x02, 0x7a, 0x02, 0xc6, 0x00, 0x0b, 0x00, 0x17, 0x00, 0x00,
            0x01, 0x14, 0x06, 0x23, 0x22, 0x26, 0x35, 0x34, 0x36, 0x33, 0x32, 0x16, 0x05, 0x14,
            0x16, 0x33, 0x32, 0x36, 0x35, 0x34, 0x26, 0x23, 0x22, 0x06, 0x02, 0x7a, 0x96, 0x98,
            0x97, 0x97, 0x97, 0x97, 0x98, 0x96, 0xfd, 0xfe, 0x6a, 0x6a, 0x6a, 0x6a, 0x6a, 0x6a,
            0x6a, 0x6a, 0x01, 0x5e, 0xb5, 0xb3, 0xb3, 0xb5, 0xb5, 0xb3, 0xb3, 0xb5, 0x8a, 0x8a,
            0x8a, 0x8a, 0x8a, 0x8a, 0x8a, 0x00, 0x00, 0x02, 0x00, 0x05, 0x00, 0x00, 0x02, 0xef,
            0x02, 0xbc, 0x00, 0x03, 0x00, 0x07, 0x00, 0x00, 0x21, 0x23, 0x01, 0x33, 0x01, 0x23,
            0x01, 0x33, 0x01, 0xb1, 0x6e, 0x01, 0x3e, 0x6e, 0xfe, 0xc2, 0x6e, 0xfe, 0xc2, 0x6e,
            0x02, 0xbc, 0xfd, 0x44, 0x02, 0xbc, 0x00, 0x03, 0x00, 0x1d, 0xff, 0xbc, 0x02, 0x44,
            0x02, 0xf7, 0x00, 0x23, 0x00, 0x2b, 0x00, 0x33, 0x00, 0x00, 0x01, 0x35, 0x33, 0x15,
            0x16, 0x16, 0x17, 0x07, 0x26, 0x26, 0x27, 0x15, 0x16, 0x16, 0x15, 0x14, 0x06, 0x07,
            0x15, 0x23, 0x35, 0x26, 0x26, 0x27, 0x37, 0x16, 0x16, 0x17, 0x35, 0x27, 0x26, 0x26,
            0x35, 0x34, 0x36, 0x36, 0x17, 0x06, 0x06, 0x15, 0x14, 0x16, 0x16, 0x17, 0x17, 0x15,
            0x36, 0x36, 0x35, 0x34, 0x26, 0x26, 0x01, 0x08, 0x5a, 0x3d, 0x68, 0x2b, 0x35, 0x24,
            0x4b, 0x2c, 0x71, 0x71, 0x79, 0x69, 0x5a, 0x48, 0x78, 0x2b, 0x34, 0x2a, 0x54, 0x39,
            0x0f, 0x65, 0x69, 0x38, 0x64, 0x41, 0x3d, 0x42, 0x17, 0x36, 0x2f, 0x5d, 0x45, 0x3f,
            0x18, 0x39, 0x02, 0xcb, 0x2c, 0x2c, 0x06, 0x2f, 0x2a, 0x48, 0x24, 0x25, 0x06, 0xfe,
            0x16, 0x58, 0x4f, 0x52, 0x6e, 0x0a, 0x32, 0x32, 0x06, 0x2e, 0x28, 0x48, 0x24, 0x25,
            0x04, 0xe8, 0x03, 0x13, 0x61, 0x52, 0x39, 0x5b, 0x39, 0x50, 0x09, 0x41, 0x33, 0x20,
            0x2a, 0x1a, 0x0a, 0x6b, 0xd8, 0x07, 0x3b, 0x30, 0x1c, 0x27, 0x19, 0x00, 0x00, 0x01,
            0x00, 0x1d, 0xff, 0xb4, 0x02, 0x44, 0x02, 0xf7, 0x00, 0x32, 0x00, 0x00, 0x01, 0x35,
            0x33, 0x15, 0x16, 0x16, 0x17, 0x07, 0x26, 0x26, 0x23, 0x22, 0x06, 0x15, 0x14, 0x16,
            0x16, 0x17, 0x17, 0x16, 0x16, 0x15, 0x14, 0x06, 0x06, 0x07, 0x15, 0x23, 0x35, 0x26,
            0x26, 0x27, 0x37, 0x1e, 0x02, 0x33, 0x32, 0x36, 0x35, 0x34, 0x26, 0x26, 0x27, 0x27,
            0x26, 0x26, 0x35, 0x34, 0x36, 0x36, 0x01, 0x08, 0x5a, 0x3d, 0x68, 0x2b, 0x35, 0x2d,
            0x5e, 0x3e, 0x52, 0x59, 0x17, 0x36, 0x2f, 0x63, 0x6d, 0x6f, 0x37, 0x65, 0x46, 0x5a,
            0x48, 0x78, 0x2b, 0x34, 0x22, 0x41, 0x4f, 0x33, 0x5d, 0x53, 0x19, 0x3c, 0x35, 0x63,
            0x65, 0x69, 0x38, 0x64, 0x02, 0xcb, 0x2c, 0x2c, 0x06, 0x2f, 0x2a, 0x48, 0x2c, 0x26,
            0x44, 0x3c, 0x20, 0x2a, 0x1a, 0x0a, 0x14, 0x16, 0x57, 0x4f, 0x36, 0x57, 0x36, 0x07,
            0x3a, 0x3a, 0x06, 0x2e, 0x28, 0x48, 0x1c, 0x23, 0x10, 0x3d, 0x37, 0x1d, 0x27, 0x1a,
            0x09, 0x12, 0x13, 0x61, 0x52, 0x39, 0x5b, 0x39, 0x00, 0x01, 0xff, 0x73, 0x02, 0x76,
            0x00, 0x7d, 0x03, 0x11, 0x00, 0x03, 0x00, 0x00, 0x13, 0x07, 0x07, 0x37, 0x7d, 0xf3,
            0x17, 0xf3, 0x03, 0x11, 0x45, 0x56, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
            0x00, 0x66, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x0f, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x06, 0x00, 0x0f, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x01, 0x00, 0x05, 0x00, 0x15, 0x00, 0x03, 0x00, 0x01, 0x04, 0x09,
            0x00, 0x01, 0x00, 0x1e, 0x00, 0x1a, 0x00, 0x03, 0x00, 0x01, 0x04, 0x09, 0x00, 0x10,
            0x00, 0x1e, 0x00, 0x1a, 0x00, 0x03, 0x00, 0x01, 0x04, 0x09, 0x01, 0x00, 0x00, 0x0c,
            0x00, 0x38, 0x00, 0x03, 0x00, 0x01, 0x04, 0x09, 0x01, 0x01, 0x00, 0x0a, 0x00, 0x44,
            0x53, 0x69, 0x6d, 0x70, 0x6c, 0x65, 0x20, 0x54, 0x77, 0x6f, 0x20, 0x41, 0x78, 0x69,
            0x73, 0x57, 0x65, 0x69, 0x67, 0x68, 0x74, 0x53, 0x6c, 0x61, 0x6e, 0x74, 0x00, 0x53,
            0x00, 0x69, 0x00, 0x6d, 0x00, 0x70, 0x00, 0x6c, 0x00, 0x65, 0x00, 0x20, 0x00, 0x54,
            0x00, 0x77, 0x00, 0x6f, 0x00, 0x20, 0x00, 0x41, 0x00, 0x78, 0x00, 0x69, 0x00, 0x73,
            0x00, 0x57, 0x00, 0x65, 0x00, 0x69, 0x00, 0x67, 0x00, 0x68, 0x00, 0x74, 0x00, 0x53,
            0x00, 0x6c, 0x00, 0x61, 0x00, 0x6e, 0x00, 0x74, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
            0x00, 0x24, 0x00, 0xc9, 0x00, 0x32, 0x00, 0x39, 0x00, 0x03, 0x00, 0x07, 0x01, 0x02,
            0x01, 0x03, 0x0b, 0x64, 0x6f, 0x6c, 0x6c, 0x61, 0x72, 0x2e, 0x62, 0x6f, 0x6c, 0x64,
            0x09, 0x61, 0x63, 0x75, 0x74, 0x65, 0x63, 0x6f, 0x6d, 0x62,
        ];
        let mut deserialized: font::Font = otspec::de::from_bytes(&binary_font).unwrap();
        deserialized.fully_deserialize();
        let glyf = deserialized
            .get_table(b"glyf")
            .unwrap()
            .unwrap()
            .glyf_unchecked();
        /*
        <TTGlyph name="A" xMin="5" yMin="0" xMax="751" yMax="700">
          <contour>
            <pt x="323" y="700" on="1"/>
            <pt x="641" y="0" on="1"/>
            <pt x="751" y="0" on="1"/>
            <pt x="433" y="700" on="1"/>
          </contour>
          <contour>
            <pt x="323" y="700" on="1"/>
            <pt x="5" y="0" on="1"/>
            <pt x="115" y="0" on="1"/>
            <pt x="433" y="700" on="1"/>
          </contour>
          <contour>
            <pt x="567" y="204" on="1"/>
            <pt x="567" y="284" on="1"/>
            <pt x="152" y="284" on="1"/>
            <pt x="152" y="204" on="1"/>
          </contour>
          <instructions/>
        </TTGlyph>
        */
        let cap_a = &glyf.glyphs[0];
        #[rustfmt::skip]
        assert_eq!(cap_a, &glyf::Glyph {
            xMin:5, yMin:0, xMax: 751, yMax:700,
            contours: vec![
                vec![
                    Point { x:323, y:700, on_curve: true },
                    Point { x:641, y:0, on_curve: true },
                    Point { x:751, y:0, on_curve: true },
                    Point { x:433, y:700, on_curve: true },
                ],
                vec![
                    Point { x:323, y:700, on_curve: true },
                    Point { x:5, y:0, on_curve: true },
                    Point { x:115, y:0, on_curve: true },
                    Point { x:433, y:700, on_curve: true },
                ],
                vec![
                    Point { x:567, y:204, on_curve: true },
                    Point { x:567, y:284, on_curve: true },
                    Point { x:152, y:284, on_curve: true },
                    Point { x:152, y:204, on_curve: true },
                ],
            ],
            components: vec![],
            instructions: vec![],
            overlap: false // There is, though.
        });

        /*
        <TTGlyph name="Aacute" xMin="5" yMin="0" xMax="751" yMax="915">
          <component glyphName="A" x="0" y="0" flags="0x4"/>
          <component glyphName="acutecomb" x="402" y="130" flags="0x4"/>
        </TTGlyph>
        */
        let aacute = &glyf.glyphs[1];
        assert_eq!(
            aacute.components[0],
            glyf::Component {
                glyph_index: 0,
                transformation: kurbo::Affine::new([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
                match_points: None,
                flags: ComponentFlags::ROUND_XY_TO_GRID
                    | ComponentFlags::ARGS_ARE_XY_VALUES
                    | ComponentFlags::MORE_COMPONENTS /* ttx hides these */
            }
        );

        #[rustfmt::skip]
        assert_eq!(
            aacute.components[1],
            glyf::Component {
                glyph_index: 7,
                transformation: kurbo::Affine::new([1.0, 0.0, 0.0, 1.0, 402.0, 130.0]),
                match_points: None,
                flags: glyf::ComponentFlags::ROUND_XY_TO_GRID
                    | ComponentFlags::ARGS_ARE_XY_VALUES
                    | ComponentFlags::ARG_1_AND_2_ARE_WORDS
            }
        );

        let component1_bytes = otspec::ser::to_bytes(&aacute).unwrap();
        assert_eq!(
            component1_bytes,
            vec![
                0xff, 0xff, 0x00, 0x05, 0x00, 0x00, 0x02, 0xef, 0x03, 0x93, 0x00, 0x26, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x07, 0x00, 0x07, 0x01, 0x92, 0x00, 0x82
            ]
        );

        assert!(glyf.glyphs[4].is_empty());
        let dollarbold = &glyf.glyphs[6];
        assert_eq!(dollarbold.xMin, 29);
        assert_eq!(dollarbold.yMin, -76);
        assert_eq!(dollarbold.xMax, 580);
        assert_eq!(dollarbold.yMax, 759);
        let firstpoint = dollarbold.contours[0][0];
        assert_eq!(
            firstpoint,
            Point {
                x: 264,
                y: 715,
                on_curve: true
            }
        );
    }

    #[test]
    fn test_insert_implicit_oncurves() {
        #[rustfmt::skip]
        let mut glyph = glyf::Glyph {
            xMin: 30, xMax: 634, yMin: -10, yMax: 710,
            components: vec![],
            instructions: vec![],
            overlap: false,
            contours: vec![
                vec![
                    Point {x: 634, y: 650, on_curve: true, },
                    Point {x: 634, y: 160, on_curve: false, },
                    Point {x: 484, y: -10, on_curve: false, },
                    Point {x: 332, y: -10, on_curve: true, },
                    Point {x: 181, y: -10, on_curve: false, },
                    Point {x: 30,  y: 169, on_curve: false, },
                    Point {x: 30,  y: 350, on_curve: true, },
                    Point {x: 30,  y: 531, on_curve: false, },
                    Point {x: 181, y: 710, on_curve: false, },
                    Point {x: 332, y: 710, on_curve: true, },
                ]
            ]
        };
        glyph.insert_explicit_oncurves();
        #[rustfmt::skip]
        assert_eq!(
            glyph.contours[0],
            vec![
                Point { x: 634, y: 650, on_curve: true },
                Point { x: 634, y: 160, on_curve: false },
                Point { x: 559, y: 75, on_curve: true },
                Point { x: 484, y: -10, on_curve: false },
                Point { x: 332, y: -10, on_curve: true },
                Point { x: 181, y: -10, on_curve: false },
                Point { x: 105, y: 79, on_curve: true },
                Point { x: 30, y: 169, on_curve: false },
                Point { x: 30, y: 350, on_curve: true },
                Point { x: 30, y: 531, on_curve: false },
                Point { x: 105, y: 620, on_curve: true },
                Point { x: 181, y: 710, on_curve: false },
                Point { x: 332, y: 710, on_curve: true }]
        );
    }
}
