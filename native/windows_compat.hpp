#pragma once
#include <windows.h>
#include <evntcons.h>
#include <tdh.h>
#include <condition_variable>
#include <algorithm>

// MinGW's ETW headers omit these Windows 8.1 SDK declarations (also defined by windows-sys 0.59).
#ifndef EVENT_FILTER_TYPE_EVENT_ID
#define EVENT_FILTER_TYPE_EVENT_ID 0x80000200
#define MAX_EVENT_FILTER_EVENT_ID_COUNT 64
struct EVENT_FILTER_EVENT_ID {
    BOOLEAN FilterIn;
    UCHAR Reserved;
    USHORT Count;
    USHORT Events[ANYSIZE_ARRAY];
};
#endif
#ifndef EVENT_ENABLE_PROPERTY_IGNORE_KEYWORD_0
#define EVENT_ENABLE_PROPERTY_IGNORE_KEYWORD_0 0x10
#endif

#ifdef __MINGW32__
inline constexpr DWORD D3DPRESENT_DONOTFLIP = 4;
inline constexpr DWORD D3DPRESENT_FLIPRESTART = 8;
inline constexpr DWORD D3DPRESENT_FORCEIMMEDIATE = 256;
#define TDH_INTYPE_UNICODESTRING 1
#define TDH_INTYPE_ANSISTRING 2
#define TDH_INTYPE_INT8 3
#define TDH_INTYPE_UINT8 4
#define TDH_INTYPE_INT16 5
#define TDH_INTYPE_UINT16 6
#define TDH_INTYPE_INT32 7
#define TDH_INTYPE_UINT32 8
#define TDH_INTYPE_POINTER 16
#define TDH_INTYPE_SID 19
#define TDH_INTYPE_SIZET 308
#define TDH_INTYPE_WBEMSID 310
#define DecodingSourceTlg static_cast<DECODING_SOURCE>(3)
#define TEI_PROPERTY_NAME(tei, epi) reinterpret_cast<wchar_t*>(reinterpret_cast<BYTE*>(tei) + (epi)->NameOffset)
#endif