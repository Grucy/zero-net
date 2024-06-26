use super::{
	constants::{fee::*, parachains},
	AccountId, AllPalletsWithSystem, Balances, AssetRegistry, Balance, Convert,
	CurrencyId::{self, ForeignAsset}, Currencies,
	EnsureRootOrThreeFourthsCouncil,
	ParachainInfo, ParachainSystem, PolkadotXcm,
	Runtime, RuntimeCall, RuntimeEvent, RuntimeOrigin, TreasuryAccountId, UnknownTokens, XcmpQueue,
	ZERO, PLAY, GAME, DOT, KSM
};
use codec::{Decode, Encode};
use cumulus_primitives_core::ParaId;
use sp_core::bounded::BoundedVec;
use sp_std::vec::Vec;
use frame_support::{
	match_types,
	parameter_types,
	traits::{ConstU32, Everything, Get, Nothing},
	weights::Weight,
};
use pallet_xcm::XcmPassthrough;
use polkadot_parachain::primitives::Sibling;
use xcm::latest::prelude::*;
use xcm_builder::{
	AccountId32Aliases, AllowKnownQueryResponses, AllowTopLevelPaidExecutionFrom,
	AllowSubscriptionsFrom, AllowUnpaidExecutionFrom, EnsureXcmOrigin, FixedRateOfFungible, FixedWeightBounds,
	NativeAsset, ParentIsPreset, RelayChainAsNative,
	SiblingParachainAsNative, SiblingParachainConvertsVia, SignedAccountId32AsNative,
	SignedToAccountId32, SovereignSignedViaLocation, TakeRevenue, TakeWeightCredit,
};
use xcm_executor::XcmExecutor;
use orml_asset_registry::{AssetRegistryTrader, FixedRateAssetRegistryTrader};
use orml_traits::{location::AbsoluteReserveProvider, FixedConversionRateProvider, MultiCurrency, parameter_type_with_key};
use orml_xcm_support::{DepositToAlternative, IsNativeConcrete, MultiCurrencyAdapter, MultiNativeAsset};


parameter_types! {
	pub const DotLocation: MultiLocation = MultiLocation::parent();
	pub const RelayNetwork: NetworkId = NetworkId::Polkadot;
	pub RelayChainOrigin: RuntimeOrigin = cumulus_pallet_xcm::Origin::Relay.into();
	pub UniversalLocation: InteriorMultiLocation = X2(GlobalConsensus(RelayNetwork::get()), Parachain(ParachainInfo::parachain_id().into()));
}

/// Type for specifying how a `MultiLocation` can be converted into an `AccountId`. This is used
/// when determining ownership of accounts for asset transacting and when attempting to use XCM
/// `Transact` in order to determine the dispatch Origin.
pub type LocationToAccountId = (
	// The parent (Relay-chain) origin converts to the parent `AccountId`.
	ParentIsPreset<AccountId>,
	// Sibling parachain origins convert to AccountId via the `ParaId::into`.
	SiblingParachainConvertsVia<Sibling, AccountId>,
	// Straight up local `AccountId32` origins just alias directly to `AccountId`.
	AccountId32Aliases<RelayNetwork, AccountId>,
);

pub type LocalAssetTransactor = MultiCurrencyAdapter<
	Currencies,
	UnknownTokens,
	IsNativeConcrete<CurrencyId, CurrencyIdConvert>,
	AccountId,
	LocationToAccountId,
	CurrencyId,
	CurrencyIdConvert,
	DepositToAlternative<TreasuryAccountId, Currencies, CurrencyId, AccountId, Balance>,
>;

/// This is the type we use to convert an (incoming) XCM origin into a local `Origin` instance,
/// ready for dispatching a transaction with Xcm's `Transact`. There is an `OriginKind` which can
/// biases the kind of local `Origin` it will become.
pub type XcmOriginToTransactDispatchOrigin = (
	// Sovereign account converter; this attempts to derive an `AccountId` from the origin location
	// using `LocationToAccountId` and then turn that into the usual `Signed` origin. Useful for
	// foreign chains who want to have a local sovereign account on this chain which they control.
	SovereignSignedViaLocation<LocationToAccountId, RuntimeOrigin>,
	// Native converter for Relay-chain (Parent) location; will converts to a `Relay` origin when
	// recognized.
	RelayChainAsNative<RelayChainOrigin, RuntimeOrigin>,
	// Native converter for sibling Parachains; will convert to a `SiblingPara` origin when
	// recognized.
	SiblingParachainAsNative<cumulus_pallet_xcm::Origin, RuntimeOrigin>,
	// Native signed account converter; this just converts an `AccountId32` origin into a normal
	// `RuntimeOrigin::Signed` origin of the same 32-byte value.
	SignedAccountId32AsNative<RelayNetwork, RuntimeOrigin>,
	// Xcm origins can be represented natively under the Xcm pallet's Xcm origin.
	XcmPassthrough<RuntimeOrigin>,
);

parameter_types! {
	// One XCM operation is 1_000_000_000 weight - almost certainly a conservative estimate.
	pub UnitWeightCost: Weight = Weight::from_parts(1_000_000_000, 64 * 1024);
	pub const MaxInstructions: u32 = 100;
	pub const MaxAssetsIntoHolding: u32 = 64;
}

match_types! {
	pub type ParentOrParentsExecutivePlurality: impl Contains<MultiLocation> = {
		MultiLocation { parents: 1, interior: Here } |
		MultiLocation { parents: 1, interior: X1(Plurality { id: BodyId::Executive, .. }) }
	};
}

match_types! {
	pub type SpecParachain: impl Contains<MultiLocation> = {
		MultiLocation { parents: 1, interior: X1(Parachain(parachains::acala::ID)) }
	};
}

pub type Barrier = (
	TakeWeightCredit,
	AllowTopLevelPaidExecutionFrom<Everything>,
	AllowKnownQueryResponses<PolkadotXcm>,
	AllowUnpaidExecutionFrom<ParentOrParentsExecutivePlurality>,
	AllowUnpaidExecutionFrom<SpecParachain>,
	AllowSubscriptionsFrom<Everything>
);

// todo: move this to common runtimes module
pub fn local_currency_location(key: CurrencyId) -> Option<MultiLocation> {
	Some(MultiLocation::new(
		0,
		X1(Junction::from(BoundedVec::try_from(key.encode()).ok()?)),
	))
}

parameter_types! {
	pub DotPerSecond: (AssetId, u128, u128) = (
		MultiLocation::parent().into(),
		dot_per_second(), 0
	);
	pub ZeroPerSecond: (AssetId, u128, u128) = (
		local_currency_location(ZERO).unwrap().into(),
		zero_per_second(), 0
	);
	pub PlayPerSecond: (AssetId, u128, u128) = (
		local_currency_location(PLAY).unwrap().into(),
		play_per_second(), 0
	);
	pub GamePerSecond: (AssetId, u128, u128) = (
		local_currency_location(GAME).unwrap().into(),
		game_per_second(), 0
	);
}

pub struct ToAuthor;
impl TakeRevenue for ToAuthor {
	fn take_revenue(revenue: MultiAsset) {
		if let MultiAsset {
			id: Concrete(location),
			fun: Fungible(amount),
		} = revenue
		{
			if let Some(currency_id) = CurrencyIdConvert::convert(location) {
				if let Some(author) = pallet_authorship::Pallet::<Runtime>::author() {
					let _ = Currencies::deposit(currency_id, &author, amount);
				} else {
					let _ = Currencies::deposit(currency_id, &TreasuryAccountId::get(), amount);
				}
			}
		}
	}
}

pub struct ChainFixedConversionRateProvider;
impl FixedConversionRateProvider for ChainFixedConversionRateProvider {
	fn get_fee_per_second(location: &MultiLocation) -> Option<u128> {
		let metadata = AssetRegistry::fetch_metadata_by_location(location)?;
		Some(metadata.additional.fee_per_second)
	}
}

pub type Trader = (
	FixedRateOfFungible<DotPerSecond, ToAuthor>,
	FixedRateOfFungible<PlayPerSecond, ToAuthor>,
	FixedRateOfFungible<GamePerSecond, ToAuthor>,
	FixedRateOfFungible<ZeroPerSecond, ToAuthor>,
	AssetRegistryTrader<FixedRateAssetRegistryTrader<ChainFixedConversionRateProvider>, ToAuthor>,
);

pub struct XcmConfig;
impl xcm_executor::Config for XcmConfig {
	type RuntimeCall = RuntimeCall;
	type XcmSender = XcmRouter;
	// How to withdraw and deposit an asset.
	type AssetTransactor = LocalAssetTransactor;
	type OriginConverter = XcmOriginToTransactDispatchOrigin;
	type IsReserve = MultiNativeAsset<AbsoluteReserveProvider>;
	type IsTeleporter = NativeAsset;
	type UniversalLocation = UniversalLocation;
	type Barrier = Barrier;
	type Weigher = FixedWeightBounds<UnitWeightCost, RuntimeCall, MaxInstructions>;
	type Trader = Trader;
	type ResponseHandler = PolkadotXcm;
	type AssetTrap = PolkadotXcm;
	type AssetClaims = PolkadotXcm;
	type SubscriptionService = PolkadotXcm;
	type PalletInstancesInfo = AllPalletsWithSystem;
	type MaxAssetsIntoHolding = MaxAssetsIntoHolding;
	type AssetLocker = ();
	type AssetExchanger = ();
	type FeeManager = ();
	type MessageExporter = ();
	type UniversalAliases = Nothing;
	type CallDispatcher = RuntimeCall;
	type SafeCallFilter = Everything;
}

/// No local origins on this chain are allowed to dispatch XCM sends/executions.
pub type LocalOriginToLocation = SignedToAccountId32<RuntimeOrigin, AccountId, RelayNetwork>;

/// The means for routing XCM messages which are not for local execution into the right message
/// queues.
pub type XcmRouter = (
	// Two routers - use UMP to communicate with the relay chain:
	cumulus_primitives_utility::ParentAsUmp<ParachainSystem, PolkadotXcm, ()>,
	// ..and XCMP to communicate with the sibling chains.
	XcmpQueue,
);

#[cfg(feature = "runtime-benchmarks")]
parameter_types! {
	pub ReachableDest: Option<MultiLocation> = Some(Parent.into());
}

impl pallet_xcm::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type SendXcmOrigin = EnsureXcmOrigin<RuntimeOrigin, LocalOriginToLocation>;
	type XcmRouter = XcmRouter;
	type ExecuteXcmOrigin = EnsureXcmOrigin<RuntimeOrigin, LocalOriginToLocation>;
	type XcmExecuteFilter = Nothing;
	// ^ Disable dispatchable execute on the XCM pallet.
	// Needs to be `Everything` for local testing.
	type XcmExecutor = XcmExecutor<XcmConfig>;
	type XcmTeleportFilter = Everything;
	type XcmReserveTransferFilter = Everything;
	type Weigher = FixedWeightBounds<UnitWeightCost, RuntimeCall, MaxInstructions>;
	type UniversalLocation = UniversalLocation;
	type RuntimeOrigin = RuntimeOrigin;
	type RuntimeCall = RuntimeCall;

	const VERSION_DISCOVERY_QUEUE_SIZE: u32 = 100;
	// ^ Override for AdvertisedXcmVersion default
	type AdvertisedXcmVersion = pallet_xcm::CurrentXcmVersion;
	type Currency = Balances;
	type CurrencyMatcher = ();
	type TrustedLockers = ();
	type SovereignAccountOf = LocationToAccountId;
	type MaxLockers = ConstU32<8>;
	type WeightInfo = pallet_xcm::TestWeightInfo;
	#[cfg(feature = "runtime-benchmarks")]
	type ReachableDest = ReachableDest;
}

impl cumulus_pallet_xcm::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type XcmExecutor = XcmExecutor<XcmConfig>;
}

pub fn native_currency_location(para_id: u32, key: Vec<u8>) -> Option<MultiLocation> {
	Some(MultiLocation::new(
        1,
		X2(Parachain(para_id), Junction::from(BoundedVec::try_from(key).ok()?)),
	))
}

pub struct CurrencyIdConvert;
impl Convert<CurrencyId, Option<MultiLocation>> for CurrencyIdConvert {
	fn convert(id: CurrencyId) -> Option<MultiLocation> {
		match id {
			DOT | KSM
				=> Some(MultiLocation::parent()),
			ZERO | GAME | PLAY
				=> native_currency_location(ParachainInfo::get().into(), id.encode()),
			ForeignAsset(id)
				=> AssetRegistry::multilocation(&id).unwrap_or_default(),
		}
	}
}
impl Convert<MultiLocation, Option<CurrencyId>> for CurrencyIdConvert {
	fn convert(location: MultiLocation) -> Option<CurrencyId> {

		fn decode_currency_id(key: &[u8]) -> Option<CurrencyId> {
        	if let Ok(currency_id) = CurrencyId::decode(&mut &*key) {
				// check `currency_id` is cross-chain asset
				match currency_id {
					ZERO | GAME | PLAY => Some(currency_id),
					_ => None,
				}
			} else {
				None
			}
		}

		match location.clone() {
			x if x == MultiLocation::parent() => Some(DOT),
			MultiLocation {
				parents: 1,
				interior: X2(Parachain(id), GeneralKey { data, length }),
			} if ParaId::from(id) == ParachainInfo::get() => decode_currency_id(&data[..length as usize]),
			MultiLocation {
				parents: 0,
				interior: X1(GeneralKey { data, length }),
			} => decode_currency_id(&data[..length as usize]),
			_ => None,
		}
		.or_else(|| AssetRegistry::location_to_asset_id(&location)
		.map(|id| CurrencyId::ForeignAsset(id)))
	}
}
impl Convert<MultiAsset, Option<CurrencyId>> for CurrencyIdConvert {
	fn convert(asset: MultiAsset) -> Option<CurrencyId> {
		if let MultiAsset {
			id: Concrete(location), ..
		} = asset
		{
			Self::convert(location)
		} else {
			None
		}
	}
}

parameter_types! {
	pub SelfLocation: MultiLocation = MultiLocation::new(1, X1(Parachain(ParachainInfo::get().into())));
}

pub struct AccountIdToMultiLocation;
impl Convert<AccountId, MultiLocation> for AccountIdToMultiLocation {
	fn convert(account: AccountId) -> MultiLocation {
		X1(AccountId32 {
			network: None,
			id: account.into(),
		})
		.into()
	}
}

parameter_types! {
	pub const MaxAssetsForTransfer: usize = 2;
}

parameter_type_with_key! {
	pub ParachainMinFee: |location: MultiLocation| -> Option<u128> {
		match (location.parents, location.first_interior()) {
			(1, Some(Parachain(parachains::acala::ID))) => Some(200_000_000),
			_ => None,
		}
	};
}

impl orml_xtokens::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type Balance = Balance;
	type CurrencyId = CurrencyId;
	type CurrencyIdConvert = CurrencyIdConvert;
	type AccountIdToMultiLocation = AccountIdToMultiLocation;
	type SelfLocation = SelfLocation;
	type XcmExecutor = XcmExecutor<XcmConfig>;
	type Weigher = FixedWeightBounds<UnitWeightCost, RuntimeCall, MaxInstructions>;
	type BaseXcmWeight = UnitWeightCost;
	type UniversalLocation = UniversalLocation;
	type MaxAssetsForTransfer = MaxAssetsForTransfer;
	type MinXcmFee = ParachainMinFee;
	type MultiLocationsFilter = Everything;
	type ReserveProvider = AbsoluteReserveProvider;
}

impl orml_unknown_tokens::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
}

impl orml_xcm::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type SovereignOrigin = EnsureRootOrThreeFourthsCouncil;
}
