import { PrefGrid } from "@/components/settings/PrefCard"
import { DisplayUnitsSection } from "@/components/settings/sections/DisplayUnitsSection"
import { TravelModeSection } from "@/components/settings/sections/TravelModeSection"
import { UpdateSection } from "@/components/settings/sections/UpdateSection"

export function DeviceTab() {
  return (
    <PrefGrid>
      <DisplayUnitsSection />
      <TravelModeSection />
      <UpdateSection />
    </PrefGrid>
  )
}
