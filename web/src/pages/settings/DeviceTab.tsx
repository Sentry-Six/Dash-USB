import { PrefGrid } from "@/components/settings/PrefCard"
import { DisplayUnitsSection } from "@/components/settings/sections/DisplayUnitsSection"
import { UpdateSection } from "@/components/settings/sections/UpdateSection"

export function DeviceTab() {
  return (
    <PrefGrid>
      <DisplayUnitsSection />
      <UpdateSection />
    </PrefGrid>
  )
}
