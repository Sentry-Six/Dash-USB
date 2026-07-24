import { MobileNotificationsSection } from "@/components/settings/sections/MobileNotificationsSection"

export function NotificationsTab() {
  return (
    <div className="grid items-start gap-2.5 sm:grid-cols-2">
      <MobileNotificationsSection />
    </div>
  )
}
