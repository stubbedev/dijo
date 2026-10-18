//! extractous's native parsers of scientific data formats: NetCDF, HDF4 and HDF5, GRIB, FITS and
//! ISA-Tab. They write what Tika's `tika-parser-scientific-module` writes of a file, its
//! attributes, dimensions and variables, and go beyond it where the module writes little or
//! nothing (#172). Each reads only the headers of a file, in place, never its data.

use extractous_base::*;
#[cfg(test)]
use extractous_core::Extractor;

/// The tests count allocations as the command line does, so that budgets are enforced in them,
/// and measure the peak of what each thread holds.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<extractous_core::large::Peak<std::alloc::System>> =
    memory::CountingAllocator(extractous_core::large::Peak(std::alloc::System));

/// The parsers, at the paths they have in `extractous-core`.
pub mod formats {
    pub(crate) use extractous_base::formats::*;
    #[cfg(test)]
    pub(crate) use extractous_core::parity;

    mod bytes;
    mod cdm;
    pub mod grib;
    pub mod hdf;
    pub mod netcdf;
}
