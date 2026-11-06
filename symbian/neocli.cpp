// SPDX-License-Identifier: Apache-2.0
// neocli.cpp -- NeoSymbian R3: a plain C++ Symbian client (euser only: RProcess, RSessionBase, TIpcArgs) for the
// Rust NeoEcho server (NEOSRV.EXE, crates neosrv + neoecho). It starts the server process, waits for its rendezvous,
// runs the same checks as neotest's ipc_server test across the process boundary, closes the session and reports how
// the server exited (it stops when its last session closes). Built by NeoSymbian/build from rust/neocli.cpp
// with no Rust linked (manifest library `-`).
#include <e32std.h>
#include <e32debug.h>

_LIT(KServerName, "NeoEcho");
_LIT(KServerExe, "neosrv");

enum TEchoOp { EAdd = 1, EReverse8, EUpper16, EWriteAt, ECount };

class RNeoEcho : public RSessionBase
	{
public:
	TInt Connect(TInt aMajor) { return CreateSession(KServerName, TVersion(aMajor, 0, 0), -1); }
	TInt Call(TInt aFunction, const TIpcArgs& aArgs) const { return SendReceive(aFunction, aArgs); }
	};

static TInt gPassed;
static TInt gFailed;

static void Check(TBool aOk, const char* aName, TInt aGot)
	{
	if (aOk)
		{
		++gPassed;
		RDebug::Printf("neocli: %s ... ok", aName);
		}
	else
		{
		++gFailed;
		RDebug::Printf("neocli: %s ... FAILED (%d)", aName, aGot);
		}
	}

// Start NEOSRV.EXE and wait until it has created its server (its rendezvous); aExit then completes when it exits.
static TInt StartServer(RProcess& aServer, TRequestStatus& aExit)
	{
	TInt r = aServer.Create(KServerExe, KNullDesC);
	if (r != KErrNone)
		return r;
	TRequestStatus ready;
	aServer.Rendezvous(ready);
	aServer.Logon(aExit);
	aServer.Resume();
	User::WaitForRequest(ready);
	return aServer.ExitType() == EExitPending ? ready.Int() : KErrDied;
	}

static void RunChecks(RNeoEcho& aEcho)
	{
	TInt r = aEcho.Call(EAdd, TIpcArgs(40, 2));
	Check(r == 42, "add", r);

	TPtrC8 in8(_L8("symbian"));
	TBuf8<16> out8;
	r = aEcho.Call(EReverse8, TIpcArgs(&in8, &out8));
	Check(r == 7 && out8 == _L8("naibmys"), "reverse8", r);

	TPtrC16 in16(_L16("Neo-3"));
	TBuf16<8> out16;
	r = aEcho.Call(EUpper16, TIpcArgs(&in16, &out16));
	Check(r == 5 && out16 == _L16("NEO-3"), "upper16", r);

	TBuf8<4> small;
	r = aEcho.Call(EWriteAt, TIpcArgs(&small, 0));
	Check(r == KErrNone && small == _L8("xyz"), "write_at", r);
	r = aEcho.Call(EWriteAt, TIpcArgs(&small, 2));
	Check(r == KErrOverflow, "overflow", r);

	r = aEcho.Call(99, TIpcArgs());
	Check(r == KErrNotSupported, "unknown_function", r);

	r = aEcho.Call(ECount, TIpcArgs());
	Check(r == 7, "session_count", r); // add, reverse8, upper16, write_at x2, 99, count
	}

GLDEF_C TInt E32Main()
	{
	RProcess server;
	TRequestStatus exit;
	TInt r = StartServer(server, exit);
	if (r != KErrNone)
		{
		RDebug::Printf("neocli: starting neosrv failed %d", r);
		return r;
		}

	RNeoEcho old;
	r = old.Connect(9);
	Check(r == KErrNotSupported, "version_refused", r);
	old.Close();

	RNeoEcho echo;
	r = echo.Connect(1);
	Check(r == KErrNone, "connect", r);
	if (r == KErrNone)
		RunChecks(echo);
	else
		server.Kill(KErrGeneral); // no session will ever close, so it would not exit
	echo.Close(); // the server's last session: it exits

	User::WaitForRequest(exit);
	const char* type = server.ExitType() == EExitKill ? "Kill" : server.ExitType() == EExitPanic ? "Panic" : "Other";
	RDebug::Printf("neocli: server exited %s %d", type, server.ExitReason());
	server.Close();
	RDebug::Printf("neocli: %d passed, %d failed", gPassed, gFailed);
	return gFailed;
	}
