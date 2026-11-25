// SPDX-License-Identifier: Apache-2.0
// neobench.cpp -- times euser's locks, atomics, heap and Mem::Crc32 on each CPU (NeoSymbian/docs user-side-smp.md,
// "ARMv8 profile"). The same EXE runs on a ROM with the A9 profile (LDREX/STREX + DMB, the CRC table) and one with the
// ARMv8 profile (LDA/STL, the CRC32 instruction); comparing the two decides what ships. It also checks Mem::Crc32
// against a bitwise reference for every length 0..64 at every alignment, so it is a correctness test on both ROMs.
// Output: `neobench: <test> cpu<N> <ns> ns` (median of 5), `neobench: <test> <N> threads <k> kops/s` (contended,
// median of 3), `neobench: crc ok`, `neobench: <P> passed, <F> failed`. Built by NeoSymbian/build from
// rust/neobench.cpp with no Rust linked (manifest library `-`).
#include <e32base.h>
#include <e32debug.h>
#include <e32svr.h>
#include <u32hal.h>
#include <e32atomics.h>

static TInt gPassed;
static TInt gFailed;
static TUint64 gHz;

static TInt KernelHal(TInt aFunction, TAny* a1)
	{
	return UserSvr::HalFunction(EHalGroupKernel, aFunction, a1, 0);
	}

static void Check(TBool aOk, const char* aName)
	{
	if (aOk)
		++gPassed;
	else
		++gFailed;
	RDebug::Printf("neobench: %s %s", aName, aOk ? "ok" : "FAILED");
	}

// ---- Mem::Crc32 against a bitwise reference (reflected 0xEDB88320, no inversion, as euser's table) ----

static TUint32 RefCrc(TUint32 aCrc, const TUint8* aP, TInt aLen)
	{
	while (aLen--)
		{
		aCrc ^= *aP++;
		for (TInt i = 0; i < 8; ++i)
			aCrc = (aCrc >> 1) ^ (0xEDB88320u & (0u - (aCrc & 1)));
		}
	return aCrc;
	}

static void CrcChecks()
	{
	TUint32 crc = 0xFFFFFFFFu;
	Mem::Crc32(crc, "123456789", 9);
	TBool ok = ~crc == 0xCBF43926u;	// the IEEE CRC-32 check value
	TUint8 buf[64 + 4];
	for (TInt i = 0; i < TInt(sizeof(buf)); ++i)
		buf[i] = TUint8(i * 37 + 11);
	for (TInt off = 0; off < 4; ++off)
		for (TInt len = 0; len <= 64; ++len)
			{
			TUint32 c = 0x12345678u + len;
			Mem::Crc32(c, buf + off, len);
			if (c != RefCrc(0x12345678u + len, buf + off, len))
				ok = EFalse;
			}
	Check(ok, "crc");
	}

// ---- timing ----

typedef void (*TLoop)(TInt aIters);

static TUint32 Ticks(TLoop aLoop, TInt aIters)
	{
	TUint32 t0 = User::FastCounter();
	aLoop(aIters);
	return User::FastCounter() - t0;	// counts up (A64 kernel: the generic timer); wraps every 2^32 ticks
	}

static void Sort(TUint64* aV, TInt aN)
	{
	for (TInt i = 1; i < aN; ++i)
		for (TInt j = i; j > 0 && aV[j - 1] > aV[j]; --j)
			{
			TUint64 t = aV[j];
			aV[j] = aV[j - 1];
			aV[j - 1] = t;
			}
	}

// ns per iteration x 10, median of 5 runs; iterations doubled until one run lasts at least 5 ms
static TUint64 Time(TLoop aLoop)
	{
	TInt iters = 256;
	while (Ticks(aLoop, iters) < gHz / 200 && iters < (1 << 24))
		iters <<= 1;
	TUint64 v[5];
	for (TInt r = 0; r < 5; ++r)
		v[r] = TUint64(Ticks(aLoop, iters)) * 10000000000ull / (gHz * TUint64(iters));
	Sort(v, 5);
	return v[2];
	}

static void Report(const char* aName, TInt aCpu, TLoop aLoop)
	{
	TUint64 t = Time(aLoop);
	RDebug::Printf("neobench: %s cpu%d %d.%d ns", aName, aCpu, TInt(t / 10), TInt(t % 10));
	}

// ---- the loops ----

static RFastLock gLock;
static volatile TUint32 gWord;
static TUint8 gData[4096];

static void FastLock(TInt n)
	{
	while (n--)
		{
		gLock.Wait();
		gLock.Signal();
		}
	}
static void AddAcq(TInt n) { while (n--) __e32_atomic_add_acq32(&gWord, 1); }
static void AddRel(TInt n) { while (n--) __e32_atomic_add_rel32(&gWord, 1); }
static void AddOrd(TInt n) { while (n--) __e32_atomic_add_ord32(&gWord, 1); }
static void AddRlx(TInt n) { while (n--) __e32_atomic_add_rlx32(&gWord, 1); }
static void LoadAcq(TInt n) { while (n--) __e32_atomic_load_acq32(&gWord); }
static void StoreRel(TInt n) { while (n--) __e32_atomic_store_rel32(&gWord, TUint32(n)); }
static void CasAcq(TInt n)
	{
	while (n--)
		{
		TUint32 q = gWord;
		__e32_atomic_cas_acq32(&gWord, &q, q + 1);
		}
	}
static void Alloc64(TInt n)
	{
	while (n--)
		User::Free(User::Alloc(64));
	}
static TUint32 gCrc;
static void Crc16(TInt n) { while (n--) Mem::Crc32(gCrc, gData, 16); }
static void Crc156(TInt n) { while (n--) Mem::Crc32(gCrc, gData + 1, 156); }	// an E32 image header, unaligned
static void Crc4096(TInt n) { while (n--) Mem::Crc32(gCrc, gData, 4096); }

struct TTest { const char* iName; TLoop iLoop; };
static const TTest KTests[] =
	{
	{"fastlock", FastLock}, {"add_acq", AddAcq}, {"add_rel", AddRel}, {"add_ord", AddOrd}, {"add_rlx", AddRlx},
	{"load_acq", LoadAcq}, {"store_rel", StoreRel}, {"cas_acq", CasAcq}, {"alloc64", Alloc64},
	{"crc16", Crc16}, {"crc156", Crc156}, {"crc4096", Crc4096},
	};

// ---- contended: one thread per CPU on the shared lock, or on the shared heap (its RFastLock) ----

const TInt KContendedIters = 20000;
const TInt KMaxCpus = 16;
static volatile TUint32 gGo;
static TUint32 gThreadTicks[KMaxCpus];

struct TArgs { TInt iCpu; TLoop iLoop; };

static TInt ContendedThread(TAny* aArgs)
	{
	const TArgs& a = *static_cast<TArgs*>(aArgs);
	KernelHal(EKernelHalLockThreadToCpu, (TAny*)a.iCpu);
	RThread::Rendezvous(KErrNone);
	while (!__e32_atomic_load_acq32(&gGo))
		{
		}
	gThreadTicks[a.iCpu] = Ticks(a.iLoop, KContendedIters);
	return KErrNone;
	}

// kilo-ops per second over all threads; 0 if a thread could not run
static TUint64 Contended(TLoop aLoop, TInt aCpus)
	{
	RThread t[KMaxCpus];
	TRequestStatus done[KMaxCpus];
	TArgs args[KMaxCpus];
	static TInt run;					// thread names unique per run: a closed thread's name can outlive Close()
	++run;
	TInt n = 0;
	gGo = 0;
	for (; n < aCpus; ++n)
		{
		args[n].iCpu = n;
		args[n].iLoop = aLoop;
		TBuf<16> name;
		name.Format(_L("nb%d.%d"), run, n);
		if (t[n].Create(name, ContendedThread, 0x2000, &User::Allocator(), &args[n]) != KErrNone)
			break;
		TRequestStatus rv;
		t[n].Rendezvous(rv);
		t[n].Logon(done[n]);
		t[n].Resume();
		User::WaitForRequest(rv);
		}
	__e32_atomic_store_rel32(&gGo, 1);
	TUint32 worst = 0;
	TBool ok = n == aCpus;
	for (TInt i = 0; i < n; ++i)
		{
		User::WaitForRequest(done[i]);
		ok = ok && t[i].ExitType() == EExitKill && done[i].Int() == KErrNone;
		if (gThreadTicks[i] > worst)
			worst = gThreadTicks[i];
		t[i].Close();
		}
	if (!ok || worst == 0)
		return 0;
	return TUint64(aCpus) * KContendedIters * gHz / (TUint64(worst) * 1000);
	}

static void ReportContended(const char* aName, TLoop aLoop, TInt aCpus)
	{
	TUint64 v[3];
	for (TInt r = 0; r < 3; ++r)
		v[r] = Contended(aLoop, aCpus);
	Sort(v, 3);
	Check(v[0] != 0, aName);
	RDebug::Printf("neobench: %s %d threads %d kops/s", aName, aCpus, TInt(v[1]));
	}

GLDEF_C TInt E32Main()
	{
	TInt hz = 0;
	TInt cpus = KernelHal(EKernelHalNumLogicalCpus, 0);
	if (KernelHal(EKernelHalFastCounterFrequency, &hz) < 0 || hz <= 0 || cpus <= 0 || gLock.CreateLocal() != KErrNone)
		{
		RDebug::Printf("neobench: setup FAILED (hz %d, cpus %d)", hz, cpus);
		return 1;
		}
	gHz = TUint32(hz);
	if (cpus > KMaxCpus)
		cpus = KMaxCpus;
	for (TInt i = 0; i < TInt(sizeof(gData)); ++i)
		gData[i] = TUint8(i * 7);
	CrcChecks();
	for (TInt cpu = 0; cpu < cpus; ++cpu)
		{
		if (KernelHal(EKernelHalLockThreadToCpu, (TAny*)cpu) != KErrNone)
			{
			Check(EFalse, "lock_to_cpu");
			continue;
			}
		for (TUint i = 0; i < sizeof(KTests) / sizeof(KTests[0]); ++i)
			Report(KTests[i].iName, cpu, KTests[i].iLoop);
		}
	KernelHal(EKernelHalLockThreadToCpu, (TAny*)0xffffffffu);	// KCpuAffinityAny
	ReportContended("fastlock_contended", FastLock, cpus);
	ReportContended("alloc64_contended", Alloc64, cpus);
	RDebug::Printf("neobench: %d passed, %d failed", gPassed, gFailed);
	gLock.Close();
	return gFailed;
	}
