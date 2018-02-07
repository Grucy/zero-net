// Copyright 2017 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

//! Shareable Polkadot types.

#![warn(missing_docs)]

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(feature = "std"), feature(alloc))]

extern crate rustc_hex;

extern crate serde;
extern crate byteorder;

#[cfg(feature = "std")]
extern crate twox_hash;
#[cfg(feature = "std")]
extern crate blake2_rfc;

#[macro_use]
extern crate crunchy;
#[macro_use]
extern crate fixed_hash;
#[cfg(feature = "std")]
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate uint as uint_crate;

#[cfg(feature = "std")]
extern crate core;
extern crate polkadot_codec as codec;
#[cfg(test)]
extern crate polkadot_serializer;
#[cfg(test)]
#[macro_use]
extern crate pretty_assertions;

#[cfg(not(feature = "std"))]
#[macro_use]
extern crate alloc;

// TODO: factor out to separate crate.
macro_rules! try_opt {
	($e: expr) => {
		match $e {
			Some(x) => x,
			None => return None,
		}
	}
}

mod bytes;
pub mod contract;
pub mod hash;
pub mod hexdisplay;
pub mod parachain;
pub mod relay;
pub mod uint;
pub mod validator;

#[cfg(test)]
mod tests;

#[cfg(feature = "std")]
pub mod hashing;

pub use self::hash::{H160, H256};
pub use self::uint::{U256, U512};

#[cfg(feature = "std")]
pub use hashing::{blake2_256, twox_128, twox_256};
