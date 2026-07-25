import { Wifi, EthernetPort } from "lucide-react"
import { PrefCard } from "@/components/settings/PrefCard"
import { Row } from "@/components/ui/StatusTile"
import { Pill } from "@/components/ui/Pill"
import type { PiStatus } from "@/lib/api"

interface Props {
  status: PiStatus | null
}

export function NetworkTab({ status }: Props) {
  const wifiConnected = !!status?.wifi_ssid
  const ethConnected =
    !!status?.ether_speed && status.ether_speed !== "Unknown!"

  return (
    // One unified 2-column grid for the whole tab so every card aligns to the
    // same two columns. The old layout stacked three separate containers — a
    // 50/50 grid (WiFi/Ethernet) and another grid
    // with a different breakpoint (Away/Cloud) — so column edges never lined
    // up row-to-row. Cards now pair up in order and collapse to one column
    // below lg (where two ~360px cards would get cramped).
    <div className="grid grid-cols-1 gap-2.5 lg:grid-cols-2">
      {/* Network interfaces */}
      <PrefCard
        icon={<Wifi className="h-3.5 w-3.5" />}
        halo={wifiConnected ? "accent" : "slate"}
        title="WiFi"
        badge={wifiConnected ? <Pill kind="accent">Connected</Pill> : null}
      >
        {wifiConnected && status ? (
          <>
            <div className="t-md font-semibold">{status.wifi_ssid}</div>
            <Row
              label="IP"
              value={<span className="t-mono">{status.wifi_ip || "—"}</span>}
            />
            {status.wifi_strength && (
              <Row label="Signal" value={status.wifi_strength} />
            )}
          </>
        ) : (
          <p className="t-xs">
            No WiFi configured. Use the Setup Wizard to scan and connect.
          </p>
        )}
      </PrefCard>

      <PrefCard
        icon={<EthernetPort className="h-3.5 w-3.5" />}
        halo={ethConnected ? "accent" : "slate"}
        title="Ethernet"
        badge={
          ethConnected && status ? <Pill kind="accent">{status.ether_speed}</Pill> : null
        }
      >
        {ethConnected && status ? (
          <>
            <Row
              label="IP"
              value={<span className="t-mono">{status.ether_ip || "—"}</span>}
            />
            <Row label="Link" value={status.ether_speed} />
          </>
        ) : (
          <p className="t-xs">No Ethernet link detected.</p>
        )}
      </PrefCard>

    </div>
  )
}
