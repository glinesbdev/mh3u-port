/* The runtime the translated code is built against: the state of a core and the helpers that give each PowerPC instruction its meaning.
 * Written to match mh3u-ppc's interpreter exactly (which is checked against QEMU's PowerPC 750). Generated code includes this. */
#ifndef MH3U_PPC_H
#define MH3U_PPC_H
#include <stdint.h>
#include <string.h>
#include <math.h>

#define AI static inline __attribute__((always_inline))

/* The fused multiply-adds need a correctly rounded fma. Without -mfma the compiler calls the C library's, and not every Windows one is
   exact (Debian's mingw-w64, against msvcrt, is off by a bit in some cases; llvm-mingw's, against UCRT, is right). On Windows the
   hardware instruction is used instead, in functions of their own because the rest is not built for FMA (the runtime checks that the
   processor has it before it loads the library). */
#if defined(_WIN32) && defined(__x86_64__) && !defined(__FMA__)
__attribute__((target("fma"), noinline)) static double ppc_fma(double a, double b, double c) { return __builtin_fma(a, b, c); }
__attribute__((target("fma"), noinline)) static float ppc_fmaf(float a, float b, float c) { return __builtin_fmaf(a, b, c); }
#define fma ppc_fma
#define fmaf ppc_fmaf
#endif

typedef union { uint64_t u; double d; } FR;

typedef struct Ctx {
    uint32_t r[32];
    FR f[32];
    FR ps1[32];
    uint32_t cr, xer, lr, ctr, fpscr;
    uint32_t gqr[8];
    uint32_t reservation, has_reservation;
    uint64_t tb;
    uint8_t *mem; /* guest address 0; a guest address a is at mem + a */
} Ctx;

/* what the program that runs the translated code gives it (the translated game is a library; the program sets these with game_set_host) */
typedef struct Host {
    void (*import)(Ctx *c, uint32_t number);     /* a call to a system function; the number is its place in `imports.h` */
    void (*unknown_call)(Ctx *c, uint32_t target); /* a call or jump to an address that is not the start of a translated function */
    void (*trap)(Ctx *c, uint32_t word);
    void (*unsupported)(Ctx *c, uint32_t word);
    void (*syscall)(Ctx *c);
    void (*enter)(Ctx *c, uint32_t function);
    void (*watch)(Ctx *c, uint32_t address, uint64_t value, uint32_t size); /* only called by a build made with MH3U_WATCH */
} Host;

#ifdef _WIN32
#define GAME_API __declspec(dllexport)
#else
#define GAME_API __attribute__((visibility("default")))
#endif

/* the hooks the generated code calls; the library routes them to the host (dispatch.c) and the test programs define them themselves */
void rt_syscall(Ctx *c);
void rt_trap(Ctx *c, uint32_t word);
void rt_unsupported(Ctx *c, uint32_t word);
void rt_call(Ctx *c, uint32_t target);     /* a call through a register or to an address the translator does not know */
void rt_import(Ctx *c, uint32_t number);   /* a call to a system function; the number is its place in `imports.h` */
void rt_jump(Ctx *c, uint32_t target);     /* a jump through a register */

/* A tail call to another translated function: a jump where the compiler can promise one (clang, GCC 15), so a chain of tail calls
 * does not grow the host's stack; a call and a return elsewhere. The caller has written its registers back first (SPILL). */
#if defined(__has_attribute)
#if __has_attribute(musttail)
#define TAIL(f) __attribute__((musttail)) return f(c)
#endif
#endif
#ifndef TAIL
#define TAIL(f) do { f(c); return; } while (0)
#endif

/* ---- the condition register */
AI uint32_t crbit(Ctx *c, uint32_t b) { return (c->cr >> (31 - b)) & 1; }
AI void setcrbit(Ctx *c, uint32_t b, uint32_t v) {
    uint32_t m = 1u << (31 - b);
    c->cr = v ? (c->cr | m) : (c->cr & ~m);
}
AI void setcrf(Ctx *c, uint32_t n, uint32_t v) {
    uint32_t s = 28 - 4 * n;
    c->cr = (c->cr & ~(0xfu << s)) | ((v & 0xf) << s);
}
AI uint32_t crf(Ctx *c, uint32_t n) { return (c->cr >> (28 - 4 * n)) & 0xf; }
AI void rec(Ctx *c, uint32_t v) {
    uint32_t so = (c->xer >> 31) & 1;
    setcrf(c, 0, ((int32_t)v < 0 ? 8 : (v == 0 ? 2 : 4)) | so);
}
AI uint32_t recv(Ctx *c, uint32_t v) { rec(c, v); return v; }
AI void setov(Ctx *c, int o) { if (o) c->xer |= 0xc0000000u; else c->xer &= ~0x40000000u; }
AI void setca(Ctx *c, int ca) { if (ca) c->xer |= 0x20000000u; else c->xer &= ~0x20000000u; }
AI uint32_t carry(Ctx *c) { return (c->xer >> 29) & 1; }

/* a + b + cin; sets CA if `ca`, OV (and SO) if `oe` */
AI uint32_t addx(Ctx *c, uint32_t a, uint32_t b, uint32_t cin, int ca, int oe) {
    uint64_t w = (uint64_t)a + b + cin;
    uint32_t r = (uint32_t)w;
    if (ca) setca(c, (int)(w >> 32));
    if (oe) setov(c, (int)(((a ^ r) & (b ^ r)) >> 31));
    return r;
}
AI uint32_t mullw(Ctx *c, uint32_t a, uint32_t b, int oe) {
    int64_t p = (int64_t)(int32_t)a * (int64_t)(int32_t)b;
    if (oe) setov(c, p != (int64_t)(int32_t)p);
    return (uint32_t)p;
}
AI uint32_t mulhw(uint32_t a, uint32_t b) { return (uint32_t)(((int64_t)(int32_t)a * (int64_t)(int32_t)b) >> 32); }
AI uint32_t mulhwu(uint32_t a, uint32_t b) { return (uint32_t)(((uint64_t)a * b) >> 32); }
AI uint32_t divw(Ctx *c, uint32_t a, uint32_t b, int oe) {
    int undefined = b == 0 || ((int32_t)a == INT32_MIN && (int32_t)b == -1);
    if (oe) setov(c, undefined);
    if (undefined) return (int32_t)a < 0 ? 0xffffffffu : 0;
    return (uint32_t)((int32_t)a / (int32_t)b);
}
AI uint32_t divwu(Ctx *c, uint32_t a, uint32_t b, int oe) {
    if (oe) setov(c, b == 0);
    return b == 0 ? 0 : a / b;
}
AI void cmps(Ctx *c, uint32_t f, uint32_t a, uint32_t b) {
    int32_t x = (int32_t)a, y = (int32_t)b;
    setcrf(c, f, (x < y ? 8 : (x > y ? 4 : 2)) | ((c->xer >> 31) & 1));
}
AI void cmpu(Ctx *c, uint32_t f, uint32_t a, uint32_t b) {
    setcrf(c, f, (a < b ? 8 : (a > b ? 4 : 2)) | ((c->xer >> 31) & 1));
}
AI uint32_t rotl(uint32_t a, uint32_t n) { n &= 31; return n ? (a << n) | (a >> (32 - n)) : a; }
AI uint32_t slw(uint32_t s, uint32_t b) { b &= 0x3f; return b >= 32 ? 0 : s << b; }
AI uint32_t srw(uint32_t s, uint32_t b) { b &= 0x3f; return b >= 32 ? 0 : s >> b; }
AI uint32_t srawx(Ctx *c, uint32_t s, uint32_t n) {
    int32_t v = (int32_t)s;
    uint32_t r;
    int ca;
    if (n >= 32) { r = v < 0 ? 0xffffffffu : 0; ca = v < 0; }
    else { r = (uint32_t)(v >> n); ca = v < 0 && n > 0 && (s & ((1u << n) - 1)) != 0; }
    setca(c, ca);
    return r;
}
AI uint32_t cntlzw(uint32_t v) { return v ? (uint32_t)__builtin_clz(v) : 32; }
AI uint32_t ext8(uint32_t v) { return (uint32_t)(int32_t)(int8_t)v; }
AI uint32_t ext16(uint32_t v) { return (uint32_t)(int32_t)(int16_t)v; }
AI uint32_t bswap32(uint32_t v) { return __builtin_bswap32(v); }

/* the branch condition of bc, bclr and bcctr: BO, BI; decrements CTR unless BO says not to */
AI int bcond(Ctx *c, uint32_t bo, uint32_t bi, int use_ctr) {
    if (use_ctr && !(bo & 4)) c->ctr -= 1;
    int ctr_ok = (bo & 4) || ((c->ctr != 0) != ((bo & 2) != 0));
    int cond_ok = (bo & 16) || (crbit(c, bi) == ((bo & 8) != 0));
    return ctr_ok && cond_ok;
}

/* ---- memory (big-endian guest) */
AI uint8_t *H(Ctx *c, uint32_t a) { return c->mem + a; }
AI uint32_t ld8(Ctx *c, uint32_t a) { return *H(c, a); }
AI uint32_t ld16(Ctx *c, uint32_t a) { uint16_t v; memcpy(&v, H(c, a), 2); return __builtin_bswap16(v); }
AI uint32_t ld16s(Ctx *c, uint32_t a) { return ext16(ld16(c, a)); }
AI uint32_t ld32(Ctx *c, uint32_t a) { uint32_t v; memcpy(&v, H(c, a), 4); return bswap32(v); }
AI uint64_t ld64(Ctx *c, uint32_t a) { uint64_t v; memcpy(&v, H(c, a), 8); return __builtin_bswap64(v); }
#ifdef MH3U_WATCH
/* a debugging build reports every store that overlaps the watched range (see game_set_watch) */
extern uint32_t g_watch_lo, g_watch_hi;
void rt_watch(Ctx *c, uint32_t a, uint64_t v, uint32_t size);
#define WATCH(c, a, v, n) do { if ((a) < g_watch_hi && (a) + (n) > g_watch_lo) rt_watch(c, a, v, n); } while (0)
extern uint32_t g_enter;
void rt_enter(Ctx *c, uint32_t function);
extern volatile uint32_t g_last_fn;
extern uint32_t g_enter_lr;
#define ENTER(c, f) do { g_last_fn = (f); if ((f) == g_enter || (c)->lr == g_enter_lr) rt_enter(c, f); } while (0)
#else
#define WATCH(c, a, v, n) do { } while (0)
#define ENTER(c, f) do { } while (0)
#endif
AI void st8(Ctx *c, uint32_t a, uint32_t v) { WATCH(c, a, v, 1); *H(c, a) = (uint8_t)v; }
AI void st16(Ctx *c, uint32_t a, uint32_t v) { WATCH(c, a, v, 2); uint16_t s = __builtin_bswap16((uint16_t)v); memcpy(H(c, a), &s, 2); }
AI void st32(Ctx *c, uint32_t a, uint32_t v) { WATCH(c, a, v, 4); uint32_t s = bswap32(v); memcpy(H(c, a), &s, 4); }
AI void st64(Ctx *c, uint32_t a, uint64_t v) { WATCH(c, a, v, 8); uint64_t s = __builtin_bswap64(v); memcpy(H(c, a), &s, 8); }
AI uint32_t ldmb(Ctx *c, uint32_t a, uint32_t k) { return ld8(c, a + k); }
AI void zero32(Ctx *c, uint32_t a) { memset(H(c, a & ~31u), 0, 32); }

/* ---- floating point */
AI double quietd(double x) { FR r; r.d = x; r.u |= 0x0008000000000000ull; return r.d; }
AI float quietf(float x) { uint32_t u; memcpy(&u, &x, 4); u |= 0x00400000u; memcpy(&x, &u, 4); return x; }
AI double defaultnan(void) { FR r; r.u = 0x7ff8000000000000ull; return r.d; }
AI float defaultnanf(void) { uint32_t u = 0x7fc00000u; float x; memcpy(&x, &u, 4); return x; }
/* a NaN made by an invalid operation is the positive quiet NaN; a NaN operand is passed on, made quiet, the first one winning */
AI double canon3(double r, double a, double b, double c) {
    if (!isnan(r)) return r;
    if (isnan(a)) return quietd(a);
    if (isnan(b)) return quietd(b);
    if (isnan(c)) return quietd(c);
    return defaultnan();
}
AI float canon3f(float r, float a, float b, float c) {
    if (!isnan(r)) return r;
    if (isnan(a)) return quietf(a);
    if (isnan(b)) return quietf(b);
    if (isnan(c)) return quietf(c);
    return defaultnanf();
}
AI double negif(double r) { return isnan(r) ? r : -r; }
AI float negiff(float r) { return isnan(r) ? r : -r; }
AI double sgl(double d) { return (double)(float)d; }
AI float tof(double d) { return (float)d; }
AI double widen(uint32_t bits) {
    float f; memcpy(&f, &bits, 4);
    if (isnan(f)) { FR r; r.u = ((uint64_t)(bits >> 31) << 63) | 0x7ff0000000000000ull | ((uint64_t)(bits & 0x007fffffu) << 29); return r.d; }
    return (double)f;
}
AI uint32_t narrow(uint64_t bits) {
    FR r; r.u = bits;
    if (isnan(r.d)) return ((uint32_t)(bits >> 63) << 31) | 0x7f800000u | ((uint32_t)(bits >> 29) & 0x007fffffu);
    float f = (float)r.d; uint32_t u; memcpy(&u, &f, 4); return u;
}
AI uint32_t cvt_i32(double x, int tr) {
    if (isnan(x)) return 0x80000000u;
    double r = tr ? trunc(x) : nearbyint(x);
    if (r >= 2147483648.0) return 0x7fffffffu;
    if (r < -2147483648.0) return 0x80000000u;
    return (uint32_t)(int32_t)r;
}
AI void fcmp(Ctx *c, uint32_t f, double a, double b) {
    setcrf(c, f, isnan(a) || isnan(b) ? 1 : (a < b ? 8 : (a > b ? 4 : 2)));
}

/* ---- paired singles (the translator reads and writes the halves itself, `f` the first and `ps1` the second, so they can be locals) */
AI int32_t sat32(float x) {
    if (isnan(x)) return INT32_MIN;
    if (x >= 2147483648.0f) return INT32_MAX;
    if (x <= -2147483648.0f) return INT32_MIN;
    return (int32_t)x;
}
AI int32_t clampi(int32_t v, int32_t lo, int32_t hi) { return v < lo ? lo : (v > hi ? hi : v); }
AI int qscale(uint32_t field) { return (int)((int32_t)(field << 26) >> 26); }
AI uint32_t qsize(uint32_t kind) { return (kind == 4 || kind == 6) ? 1 : ((kind == 5 || kind == 7) ? 2 : 4); }
AI uint64_t qread(Ctx *c, uint32_t at, uint32_t kind, int scale) {
    FR r;
    switch (kind) {
    case 4: r.d = (double)((float)ld8(c, at) * ldexpf(1.0f, -scale)); return r.u;
    case 5: r.d = (double)((float)ld16(c, at) * ldexpf(1.0f, -scale)); return r.u;
    case 6: r.d = (double)((float)(int8_t)ld8(c, at) * ldexpf(1.0f, -scale)); return r.u;
    case 7: r.d = (double)((float)(int16_t)ld16(c, at) * ldexpf(1.0f, -scale)); return r.u;
    default: {
        uint32_t bits = ld32(c, at);
        float f; memcpy(&f, &bits, 4);
        if (isnan(f)) return ((uint64_t)(bits >> 31) << 63) | 0x7ff0000000000000ull | ((uint64_t)(bits & 0x007fffffu) << 29);
        r.d = (double)f; return r.u;
    }
    }
}
AI void psq_load(Ctx *c, FR *lo, FR *hi, uint32_t ea, int w, int g) {
    uint32_t kind = (c->gqr[g] >> 16) & 7;
    int scale = qscale(c->gqr[g] >> 24);
    uint64_t first = qread(c, ea, kind, scale);
    FR one; one.d = 1.0;
    uint64_t second = w ? one.u : qread(c, ea + qsize(kind), kind, scale);
    lo->u = first;
    hi->u = second;
}
AI void qwrite(Ctx *c, uint32_t at, uint64_t bits, uint32_t kind, int scale) {
    FR v; v.u = bits;
    float x = (float)v.d;
    int32_t q = sat32(x * ldexpf(1.0f, scale));
    switch (kind) {
    case 4: st8(c, at, (uint32_t)clampi(q, 0, 255)); break;
    case 5: st16(c, at, (uint32_t)clampi(q, 0, 65535)); break;
    case 6: st8(c, at, (uint32_t)(uint8_t)(int8_t)clampi(q, -128, 127)); break;
    case 7: st16(c, at, (uint32_t)(uint16_t)(int16_t)clampi(q, -32768, 32767)); break;
    default: st32(c, at, narrow(bits)); break;
    }
}
AI void psq_store(Ctx *c, uint64_t first, uint64_t second, uint32_t ea, int w, int g) {
    uint32_t kind = c->gqr[g] & 7;
    int scale = qscale(c->gqr[g] >> 8);
    qwrite(c, ea, first, kind, scale);
    if (!w) qwrite(c, ea + qsize(kind), second, kind, scale);
}
#endif
