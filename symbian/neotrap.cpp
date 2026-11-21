// SPDX-License-Identifier: Apache-2.0
// neotrap.cpp -- a TRAP keeps d8-d15 across a leave (NeoSymbian/docs user-side-smp.md US2). Code built with VFP (the
// ARMv8 user side) keeps values in d8-d15, which the calling convention says a call preserves; a leave skips the
// epilogues that would restore them, so its TRAP (NeoSymbian/build toolchain/overlay/e32cmn.h) traps through euser's
// NeoTrapFp, which saves d8-d15 after the TTrap, and the leave puts them back. This program is built without VFP, so
// the compiler never touches d8-d15 and the registers hold exactly what the assembly helpers put there. Built by
// NeoSymbian/build from rust/neotrap.cpp with no Rust linked (manifest library `-`).
#include <e32base.h>
#include <e32debug.h>

extern "C" TInt NeoTrapFp(TTrap* aThis, TInt& aResult) __attribute__((returns_twice));
extern "C" TInt _ZN5TTrap4TrapERi(TTrap* aThis, TInt& aResult) __attribute__((returns_twice));	// TTrap::Trap
struct TFrame { TTrap iTrap; TUint64 iFp[8]; };

// ARM state: the Thumb of an ARMv5 build has no VFP instructions
__attribute__((target("arm"), noinline)) static void LoadFp(const TUint64* aFrom)
	{
	asm volatile(".fpu vfpv2\n\tvldmia %0, {d8-d15}" : : "r"(aFrom) : "memory");
	}

__attribute__((target("arm"), noinline)) static void StoreFp(TUint64* aTo)
	{
	asm volatile(".fpu vfpv2\n\tvstmia %0, {d8-d15}" : : "r"(aTo) : "memory");
	}

static void Fill(TUint64* aRegs, TUint32 aSeed)
	{
	for (TInt i = 0; i < 8; ++i)
		aRegs[i] = (TUint64(aSeed + i) << 32) | (aSeed ^ (0x1111u * (i + 1)));
	}

// what a deeper function does before it leaves: d8-d15 now hold its own values
__attribute__((noinline)) static void ClobberAndLeave(TInt aReason)
	{
	TUint64 junk[8];
	Fill(junk, 0xBAD00000);
	LoadFp(junk);
	User::Leave(aReason);
	}

static TBool Same(const TUint64* aA, const TUint64* aB)
	{
	for (TInt i = 0; i < 8; ++i)
		if (aA[i] != aB[i])
			return EFalse;
	return ETrue;
	}

static TInt gPassed;
static TInt gFailed;

static void Check(TBool aOk, const char* aName, TInt aGot)
	{
	if (aOk)
		{
		++gPassed;
		RDebug::Printf("test %s ... ok", aName);
		}
	else
		{
		++gFailed;
		RDebug::Printf("test %s ... FAILED (%d)", aName, aGot);
		}
	}

// a leave returns to NeoTrapFp's frame with d8-d15 as they were at the TRAP
static void LeaveRestores()
	{
	TUint64 want[8], got[8];
	Fill(want, 0xA0000000);
	TFrame f;
	TInt r;
	LoadFp(want);
	if (NeoTrapFp(&f.iTrap, r) == 0)
		{
		ClobberAndLeave(-7);
		TTrap::UnTrap();
		}
	StoreFp(got);
	Check(r == -7 && Same(want, got), "leave_restores_d8_d15", r);
	}

// no leave: the registers simply survive
static void NoLeave()
	{
	TUint64 want[8], got[8];
	Fill(want, 0xB0000000);
	TFrame f;
	TInt r;
	LoadFp(want);
	if (NeoTrapFp(&f.iTrap, r) == 0)
		TTrap::UnTrap();
	StoreFp(got);
	Check(r == KErrNone && Same(want, got), "no_leave", r);
	}

// an inner trap returns its own values, the outer one its own
static void Nested()
	{
	TUint64 outer[8], inner[8], got[8];
	Fill(outer, 0xC0000000);
	Fill(inner, 0xD0000000);
	TFrame fo, fi;
	TInt ro, ri = 0;
	TBool innerOk = EFalse;
	LoadFp(outer);
	if (NeoTrapFp(&fo.iTrap, ro) == 0)
		{
		LoadFp(inner);
		if (NeoTrapFp(&fi.iTrap, ri) == 0)
			{
			ClobberAndLeave(-1);
			TTrap::UnTrap();
			}
		StoreFp(got);
		innerOk = ri == -1 && Same(inner, got);
		ClobberAndLeave(-2);
		TTrap::UnTrap();
		}
	StoreFp(got);
	Check(innerOk && ro == -2 && Same(outer, got), "nested", ro);
	}

// TTrap::Trap clears the mark: a frame reused after NeoTrapFp restores nothing (only NeoTrapFp's frames have room)
static void PlainTrapClearsMark()
	{
	TUint64 a[8], got[8], junk[8];
	Fill(a, 0xE0000000);
	Fill(junk, 0xBAD00000);
	TFrame f;
	TInt r;
	LoadFp(a);
	if (NeoTrapFp(&f.iTrap, r) == 0)
		{
		ClobberAndLeave(-3);
		TTrap::UnTrap();
		}
	if (_ZN5TTrap4TrapERi(&f.iTrap, r) == 0)
		{
		ClobberAndLeave(-4);
		TTrap::UnTrap();
		}
	StoreFp(got);
	Check(r == -4 && Same(junk, got), "plain_trap_clears_mark", r);
	}

GLDEF_C TInt E32Main()
	{
	CTrapCleanup* cleanup = CTrapCleanup::New();
	LeaveRestores();
	NoLeave();
	Nested();
	PlainTrapClearsMark();
	RDebug::Printf("neotrap: %d passed, %d failed", gPassed, gFailed);
	delete cleanup;
	return gFailed;
	}
