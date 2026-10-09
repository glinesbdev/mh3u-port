// Prints tiling test vectors from AMD's address library (MIT; https://github.com/decaf-emu/addrlib, the R6xx/R7xx version) for the
// console's configuration. Not built by cargo; see the comment at the top of src/tiling_vectors.txt for how to run it.
#include <addrlib/addrinterface.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
static void *alloc_(const ADDR_ALLOCSYSMEM_INPUT *p) { return malloc(p->sizeInBytes); }
static ADDR_E_RETURNCODE free_(const ADDR_FREESYSMEM_INPUT *p) { free(p->pVirtAddr); return ADDR_OK; }
int main() {
  ADDR_CREATE_INPUT ci; ADDR_CREATE_OUTPUT co; memset(&ci,0,sizeof ci); memset(&co,0,sizeof co);
  ci.size=sizeof ci; co.size=sizeof co; ci.chipEngine=CIASICIDGFXENGINE_R600; ci.chipFamily=0x51; ci.chipRevision=71;
  ci.createFlags.fillSizeFields=1; ci.regValue.gbAddrConfig=0x44902; ci.callbacks.allocSysMem=alloc_; ci.callbacks.freeSysMem=free_;
  if (AddrCreate(&ci,&co)!=ADDR_OK) return 1; ADDR_HANDLE h=co.hLib;
  int modes[]={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15};
  struct F{int fmt,bpp;} fmts[]={{0x01,8},{0x07,16},{0x1a,32},{0x1e,64},{0x23,128},{0x31,64},{0x33,128}};
  struct S{unsigned w,h,slices; int cube;} sizes[]={{1,1,1,0},{5,3,1,0},{16,16,1,0},{64,64,1,0},{100,60,1,0},{256,256,1,0},{300,200,1,0},{64,64,6,1},{32,32,4,0},{128,32,8,0}};
  unsigned swz[]={0,3,6};
  for (int m: modes) for (auto f: fmts) for (auto s: sizes) for (unsigned level=0; level<4; level++) for (unsigned b: swz) {
    if (s.w>>level==0 && s.h>>level==0 && level) continue;
    if (b && (m<4)) continue;
    unsigned w = s.w>>level ? s.w>>level : 1, hh = s.h>>level ? s.h>>level : 1;
    ADDR_COMPUTE_SURFACE_INFO_INPUT in; ADDR_COMPUTE_SURFACE_INFO_OUTPUT out; memset(&in,0,sizeof in); memset(&out,0,sizeof out);
    in.size=sizeof in; out.size=sizeof out; in.tileMode=(AddrTileMode)m; in.format=(AddrFormat)f.fmt; in.bpp=f.bpp; in.width=w; in.height=hh;
    in.numSlices=s.slices; in.numSamples=1; in.numFrags=1; in.mipLevel=level; in.flags.cube=s.cube; in.flags.inputBaseMap=(level==0);
    if (AddrComputeSurfaceInfo(h,&in,&out)!=ADDR_OK) { printf("# fail\n"); continue; }
    ADDR_EXTRACT_BANKPIPE_SWIZZLE_INPUT si; ADDR_EXTRACT_BANKPIPE_SWIZZLE_OUTPUT so; memset(&si,0,sizeof si); memset(&so,0,sizeof so);
    si.size=sizeof si; so.size=sizeof so; si.base256b=b; AddrExtractBankPipeSwizzle(h,&si,&so);
    bool comp = f.fmt>=0x31; unsigned ew = comp ? (w+3)/4 : w, eh = comp ? (hh+3)/4 : hh;
    unsigned ns = s.slices < 4 ? s.slices : 4;
    uint64_t hash=0xcbf29ce484222325ull;
    for (unsigned z=0; z<ns; z++) for (unsigned y=0; y<eh; y++) for (unsigned x=0; x<ew; x++) {
      ADDR_COMPUTE_SURFACE_ADDRFROMCOORD_INPUT ai; ADDR_COMPUTE_SURFACE_ADDRFROMCOORD_OUTPUT ao; memset(&ai,0,sizeof ai); memset(&ao,0,sizeof ao);
      ai.size=sizeof ai; ao.size=sizeof ao; ai.x=x; ai.y=y; ai.slice=z; ai.bpp=f.bpp; ai.pitch=out.pitch; ai.height=out.height;
      ai.numSlices=out.depth; ai.numSamples=1; ai.numFrags=1; ai.tileMode=out.tileMode; ai.pipeSwizzle=so.pipeSwizzle; ai.bankSwizzle=so.bankSwizzle;
      AddrComputeSurfaceAddrFromCoord(h,&ai,&ao);
      for (int i=0;i<8;i++){ hash^=(ao.addr>>(8*i))&0xff; hash*=1099511628211ull; }
    }
    printf("%d %d %#x %u %u %u %d %u %u %d %u %u %u %llu %u %016llx\n", m, f.bpp, f.fmt, w, hh, s.slices, s.cube, level, b,
      (int)out.tileMode, out.pitch, out.height, out.depth, (unsigned long long)out.surfSize, out.baseAlign, (unsigned long long)hash);
  }
}
