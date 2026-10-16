//! PDFBox's `Matrix`: an affine transform held as nine Java `float`s and computed in `float`, so
//! that the positions text extraction derives from it round as PDFBox's do.

use super::jfloat;
use super::Unported;

/// A 3×3 matrix `[a b 0; c d 0; e f 1]`, laid out as PDFBox lays it out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Matrix([f32; 9]);

impl Default for Matrix {
    fn default() -> Self {
        Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])
    }
}

impl Matrix {
    pub(super) fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self([a, b, 0.0, c, d, 0.0, e, f, 1.0])
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
        self.0[3]
    }

    pub(super) fn scale_y(&self) -> f32 {
        self.0[4]
    }

    pub(super) fn translate_x(&self) -> f32 {
        self.0[6]
    }

    pub(super) fn translate_y(&self) -> f32 {
        self.0[7]
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
        m[6] += tx * m[0] + ty * m[3];
        m[7] += tx * m[1] + ty * m[4];
        m[8] += tx * m[2] + ty * m[5];
        checked(*m).map(|_| ())
    }

    /// `Matrix.transformPoint`.
    pub(super) fn transform_point(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.0;
        (x * m[0] + y * m[3] + m[6], x * m[1] + y * m[4] + m[7])
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
        if !jfloat::same(self.0[3], 0.0) {
            hypot(self.0[3], self.0[4])
        } else {
            self.0[4]
        }
    }
}

/// `(float) Math.sqrt(Math.pow(x, 2) + Math.pow(y, 2))`.
fn hypot(x: f32, y: f32) -> f32 {
    let (x, y) = (f64::from(x), f64::from(y));
    (x * x + y * y).sqrt() as f32
}

fn multiply(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
    [
        a[0] * b[0] + a[1] * b[3] + a[2] * b[6],
        a[0] * b[1] + a[1] * b[4] + a[2] * b[7],
        a[0] * b[2] + a[1] * b[5] + a[2] * b[8],
        a[3] * b[0] + a[4] * b[3] + a[5] * b[6],
        a[3] * b[1] + a[4] * b[4] + a[5] * b[7],
        a[3] * b[2] + a[4] * b[5] + a[5] * b[8],
        a[6] * b[0] + a[7] * b[3] + a[8] * b[6],
        a[6] * b[1] + a[7] * b[4] + a[8] * b[7],
        a[6] * b[2] + a[7] * b[5] + a[8] * b[8],
    ]
}

/// `Matrix.checkFloatValues`, where PDFBox throws an `IllegalArgumentException` the port does
/// not follow through PDFBox's error handling.
fn checked(values: [f32; 9]) -> Result<[f32; 9], Unported> {
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
}
