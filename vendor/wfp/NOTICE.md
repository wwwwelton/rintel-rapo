# Third-party notice: Mudfish WFP components

`mudwfp_proxy.exe`, `MUDWFP.sys`, and `mudwfp.inf` are unmodified third-party
Mudfish components, bundled verbatim at the user's explicit request.
They are copyright Mudfish Networks (mudfish.net); this project does not
hold their license and no redistribution rights are asserted.

The local driver used for this bundle had a valid Authenticode signature
from Microsoft Windows Hardware Compatibility Publisher. Its SHA-256 is:
`f4e658f1f88fb6f3e1a0650bd0ee9f2ffec3cbb2953064f0e7ce2226972bf3e9`.
The supplied INF declares driver version `05/09/2022,22.23.0.588` and refers
to `MUDWFP.cat`, which was not available locally and is not bundled. RAPO
does not claim to provide a complete PnP driver package or use this INF
for installation: it prepares a demand-start kernel service only after
checking the pinned driver bytes and the Windows Authenticode result.

Driver files are extracted from the executable at runtime. Installing
the SYS in the protected Windows drivers directory and starting its
service require administrator permission. RAPO does not re-sign the
driver, alter Secure Boot, enable test-signing, or promise anti-cheat
compatibility. Signing the RAPO executable does not change driver trust.
