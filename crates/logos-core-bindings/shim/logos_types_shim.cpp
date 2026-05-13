// logos_types_shim.cpp
//
// Backward-compatible QDataStream operators for LogosResult.
//
// delivery_module_plugin.so was compiled with an older logos-cpp-sdk that
// serialised LogosResult with only two fields on the wire:
//
//   operator<<: success (signed char) + value (QVariant)    -- NO error field
//   operator>>: success (bool)        + value (QVariant)    -- NO error field
//
// The logos_types.cpp comment confirms this was intentional once and then
// changed: "The error field used to be dropped on the wire — senders set it,
// receivers got a default-constructed (null) QVariant."
//
// The current SDK (>= 2026-05) extended the format to three fields.  That is
// safe when "Daemon and modules always build from the same SDK", but here the
// shim (new SDK) talks to a logos_host_qt whose delivery_module was compiled
// with the old SDK.
//
// This file is compiled into liblogos_shim.a, which the final link processes
// BEFORE liblogos_sdk.a, so this definition takes precedence over the three-
// field version in the pre-built archive.
//
// operator<<: kept at three fields (matches what a new-SDK host expects when
//             deserialising inbound calls — the shim never sends LogosResult
//             values, but provide a complete definition to avoid ODR issues).
// operator>>: reads only success + value to match delivery_module's wire format.

#include "logos_types.h"
#include <QDataStream>

QDataStream &operator<<(QDataStream &out, const LogosResult &result)
{
    return out << result.success << result.value << result.error;
}

QDataStream &operator>>(QDataStream &in, LogosResult &result)
{
    // Old wire format (delivery_module_plugin.so, pre-2026 SDK):
    //   bool success  +  QVariant value   — NO error field written.
    //
    // Read exactly those two fields; leave result.error as the default-
    // constructed null QVariant.  The upstream fix is to rebuild
    // delivery_module against the same SDK as the shim; until then this
    // keeps the stream aligned regardless of which packet the LogosResult
    // is embedded in (InvokeReplyPacket, InitDynamicPacket property, etc.).
    in >> result.success >> result.value;
    result.error = QVariant();
    return in;
}
