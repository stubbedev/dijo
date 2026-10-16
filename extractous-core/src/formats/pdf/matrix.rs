//! PDFBox's `Matrix`: an affine transform computed in Java `float`, so that the positions text
//! extraction derives from it round as PDFBox's do.
//!
//! PDFBox holds nine floats, `[a b 0; c d 0; e f 1]`. Every matrix text extraction makes keeps
//! the third column at exactly +0, +0 and 1: `new Matrix(a, b, c, d, e, f)` sets it so, a
//! product's third column is `±0 ± 0 + 0 = +0` twice and `±0 ± 0 + 1 = 1`, and a translation
//! adds `±0` to 1. So the six other floats are held, and each of them is computed with the very
//! operations PDFBox's nine-float arithmetic does, the third column's `+0` and `1` included,
//! since they decide the sign of a zero.

use super::jfloat;
use super::Unported;

/// The affine matrix `[a b 0; c d 0; e f 1]`, as `[a, b, c, d, e, f]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Matrix([f32; 6]);

impl Default for Matrix {
    fn default() -> Self {
        Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
    }
}

impl Matrix {
    pub(super) fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self([a, b, c, d, e, f])
    }

    pub(super) fn translation(x: f32, y: f32) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    pub(super) fn scale_x(&self) -> f32 {
        self.0[0]
    }

    pub(super) fn shear_y(&self) -> f32 {
        self.0[1]
    }

    pub(super) fn shear_x(&self) -> f32 {
        self.0[2]
    }

    pub(super) fn scale_y(&self) -> f32 {
        self.0[3]
    }

    pub(super) fn translate_x(&self) -> f32 {
        self.0[4]
    }

    pub(super) fn translate_y(&self) -> f32 {
        self.0[5]
    }

    /// `Matrix.multiply`: `self × other`, failing as PDFBox does on a value that is not finite.
    pub(super) fn multiply(&self, other: &Matrix) -> Result<Matrix, Unported> {
        Ok(Matrix(checked(multiply(&self.0, &other.0))?))
    }

    /// `Matrix.concatenate`: this matrix becomes `other × self`.
    pub(super) fn concatenate(&mut self, other: &Matrix) -> Result<(), Unported> {
        self.0 = checked(multiply(&other.0, &self.0))?;
        Ok(())
    }

    /// `Matrix.translate(tx, ty)`.
    pub(super) fn translate(&mut self, tx: f32, ty: f32) -> Result<(), Unported> {
        let m = &mut self.0;
        m[4] += tx * m[0] + ty * m[2];
        m[5] += tx * m[1] + ty * m[3];
        checked(*m).map(|_| ())
    }

    /// `Matrix.transformPoint`.
    pub(super) fn transform_point(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.0;
        (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
    }

    /// `Matrix.getScalingFactorX`.
    pub(super) fn scaling_factor_x(&self) -> f32 {
        if !jfloat::same(self.0[1], 0.0) {
            hypot(self.0[0], self.0[1])
        } else {
            self.0[0]
        }
    }

    /// `Matrix.getScalingFactorY`.
    pub(super) fn scaling_factor_y(&self) -> f32 {
        if !jfloat::same(self.0[2], 0.0) {
            hypot(self.0[2], self.0[3])
        } else {
            self.0[3]
        }
    }
}

/// `(float) Math.sqrt(Math.pow(x, 2) + Math.pow(y, 2))`.
fn hypot(x: f32, y: f32) -> f32 {
    let (x, y) = (f64::from(x), f64::from(y));
    (x * x + y * y).sqrt() as f32
}

/// The third column of every matrix, `[0, 0, 1]`, as PDFBox's products read it.
const ZERO: f32 = 0.0;
const ONE: f32 = 1.0;

/// PDFBox's product of two nine-float matrices, `a × b`, for the six floats it does not keep
/// constant, term by term in its order.
fn multiply(a: &[f32; 6], b: &[f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[1] * b[2] + ZERO * b[4],
        a[0] * b[1] + a[1] * b[3] + ZERO * b[5],
        a[2] * b[0] + a[3] * b[2] + ZERO * b[4],
        a[2] * b[1] + a[3] * b[3] + ZERO * b[5],
        a[4] * b[0] + a[5] * b[2] + ONE * b[4],
        a[4] * b[1] + a[5] * b[3] + ONE * b[5],
    ]
}

/// `Matrix.checkFloatValues`, where PDFBox throws an `IllegalArgumentException` the port does
/// not follow through PDFBox's error handling.
fn checked(values: [f32; 6]) -> Result<[f32; 6], Unported> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(values)
    } else {
        Err(Unported(
            "a transformation matrix becomes infinite, which PDFBox throws on".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenates_and_translates_in_float() {
        let mut m = Matrix::new(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);
        m.concatenate(&Matrix::translation(1.0, 1.0)).unwrap();
        assert_eq!(m, Matrix::new(2.0, 0.0, 0.0, 3.0, 12.0, 23.0));
        m.translate(1.0, 2.0).unwrap();
        assert_eq!(m.transform_point(0.0, 0.0), (14.0, 29.0));
        let product = Matrix::translation(5.0, 0.0).multiply(&m).unwrap();
        assert_eq!(product.translate_x(), 24.0);
    }

    #[test]
    fn scaling_factors_follow_rotation() {
        let rotated = Matrix::new(0.0, 2.0, -2.0, 0.0, 0.0, 0.0);
        assert_eq!(rotated.scaling_factor_x(), 2.0);
        assert_eq!(rotated.scaling_factor_y(), 2.0);
        assert_eq!(
            Matrix::new(3.0, 0.0, 0.0, 4.0, 0.0, 0.0).scaling_factor_y(),
            4.0
        );
    }

    #[test]
    fn refuses_infinite_values() {
        let big = Matrix::new(f32::MAX, 0.0, 0.0, f32::MAX, 0.0, 0.0);
        assert!(big.multiply(&big).is_err());
    }

    /// PDFBox's `Matrix`, nine floats, as the reference for the six-float one.
    #[derive(Clone, Copy)]
    struct Pdfbox([f32; 9]);

    impl Pdfbox {
        fn of(m: &Matrix) -> Self {
            let [a, b, c, d, e, f] = m.0;
            Pdfbox([a, b, 0.0, c, d, 0.0, e, f, 1.0])
        }

        fn multiply(&self, other: &Pdfbox) -> Pdfbox {
            let (a, b) = (&self.0, &other.0);
            Pdfbox([
                a[0] * b[0] + a[1] * b[3] + a[2] * b[6],
                a[0] * b[1] + a[1] * b[4] + a[2] * b[7],
                a[0] * b[2] + a[1] * b[5] + a[2] * b[8],
                a[3] * b[0] + a[4] * b[3] + a[5] * b[6],
                a[3] * b[1] + a[4] * b[4] + a[5] * b[7],
                a[3] * b[2] + a[4] * b[5] + a[5] * b[8],
                a[6] * b[0] + a[7] * b[3] + a[8] * b[6],
                a[6] * b[1] + a[7] * b[4] + a[8] * b[7],
                a[6] * b[2] + a[7] * b[5] + a[8] * b[8],
            ])
        }

        fn translate(&mut self, tx: f32, ty: f32) {
            let m = &mut self.0;
            m[6] += tx * m[0] + ty * m[3];
            m[7] += tx * m[1] + ty * m[4];
            m[8] += tx * m[2] + ty * m[5];
        }

        /// The bits of the nine floats, with the third column checked to be `[+0, +0, 1]`.
        fn bits(&self) -> [u32; 6] {
            let m = &self.0;
            assert_eq!(
                [m[2].to_bits(), m[5].to_bits(), m[8].to_bits()],
                [0.0f32.to_bits(), 0.0f32.to_bits(), 1.0f32.to_bits()]
            );
            [m[0], m[1], m[3], m[4], m[6], m[7]].map(f32::to_bits)
        }
    }

    fn bits(m: &Matrix) -> [u32; 6] {
        m.0.map(f32::to_bits)
    }

    #[test]
    fn computes_every_bit_as_pdfbox_s_nine_floats_do() {
        let values = [
            0.0, -0.0, 1.0, -1.0, 0.5, -2.25, 1e-30, -3e-41, 12.75, 1e20, -7.1e-3, 3.4e38,
        ];
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut pick = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            values[(state % values.len() as u64) as usize]
        };
        for _ in 0..20_000 {
            let a = Matrix::new(pick(), pick(), pick(), pick(), pick(), pick());
            let b = Matrix::new(pick(), pick(), pick(), pick(), pick(), pick());
            let reference = Pdfbox::of(&a).multiply(&Pdfbox::of(&b));
            match a.multiply(&b) {
                Ok(product) => assert_eq!(bits(&product), reference.bits()),
                Err(_) => assert!(reference.0.iter().any(|value| !value.is_finite())),
            }
            let (tx, ty) = (pick(), pick());
            let mut translated = a;
            let mut reference = Pdfbox::of(&a);
            reference.translate(tx, ty);
            match translated.translate(tx, ty) {
                Ok(()) => assert_eq!(bits(&translated), reference.bits()),
                Err(_) => assert!(reference.0.iter().any(|value| !value.is_finite())),
            }
        }
    }
}
