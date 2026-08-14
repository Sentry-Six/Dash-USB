# Notifications

Dash USB can send you push notifications when:

- An archive run starts
- An archive completes or fails (with file counts and duration)
- The Pi's temperature crosses your Warning / Caution thresholds
- A post-archive temperature report, when enabled
- Storage repair completes or requires manual action

Configure providers in the [Setup Wizard](Setup-Wizard-Guide#6-notifications) — re-run it anytime from **Settings → System → Setup Wizard**. Mobile app push pairing lives in **Settings → Notifications**.

You can enable multiple providers. Credential-based providers are enabled when
their fields are filled; Mobile App uses an explicit checkbox.

## Providers

### Pushover
iOS and Android notification service.

| Field | Where to get it |
|-------|-----------------|
| User Key | Pushover dashboard → your user key |
| App Key | Create an Application in the Pushover dashboard |

### ntfy
Free, self-hostable push notifications. Subscribe to a topic on the [ntfy.sh](https://ntfy.sh) iOS / Android app.

| Field | Example |
|-------|---------|
| URL & Topic | `https://ntfy.sh/your-secret-topic` |
| Access Token | (optional, only for self-hosted with auth) |
| Priority | `3` (1=lowest, 5=highest) |

### Gotify
Self-hosted push notification server. You run Gotify on a home server; the Android app receives the push.

| Field | Example |
|-------|---------|
| Domain | `https://gotify.example.com` |
| App Token | Created in the Gotify web UI under Apps |
| Priority | `5` |

### Discord
Posts to a Discord channel via a webhook.

| Field | Where to get it |
|-------|-----------------|
| Webhook URL | Server Settings → Integrations → Webhooks → New Webhook → Copy URL |

### Telegram
Posts via a Telegram bot.

| Field | Where to get it |
|-------|-----------------|
| Chat ID | Send a message to your bot, then visit `https://api.telegram.org/bot<TOKEN>/getUpdates` |
| Bot Token | Create a bot with [@BotFather](https://t.me/botfather) |

### Slack
Posts to a Slack channel via an Incoming Webhook.

| Field | Where to get it |
|-------|-----------------|
| Webhook URL | Slack App settings → Incoming Webhooks → Add to channel |

### Signal
Sends via [signal-cli](https://github.com/AsamK/signal-cli) running on your network.

| Field | Example |
|-------|---------|
| Signal CLI URL | `http://signal-host:8080` |
| From Number | `+15555550100` |
| To Number | `+15555550199` |

### Matrix
Posts to a Matrix room.

| Field | Example |
|-------|---------|
| Server URL | `https://matrix.org` |
| Username | `yourname` |
| Password | _your password_ |
| Room ID | `!roomid:matrix.org` |

### AWS SNS
For automation / cloud workflows.

| Field | Example |
|-------|---------|
| Region | `us-east-1` |
| Access Key ID | `AKIA...` |
| Secret Key | _your secret_ |
| Topic ARN | `arn:aws:sns:us-east-1:123:my-topic` |

### IFTTT
Triggers an IFTTT applet by sending an event.

| Field | Where to get it |
|-------|-----------------|
| Event Name | Whatever you named the event in your applet |
| Key | IFTTT Webhooks service → Documentation page → your key |

### Webhook
Generic — POSTs a JSON payload to any URL. Useful for Home Assistant, n8n, Node-RED, etc.

| Field | Example |
|-------|---------|
| Webhook URL | `http://homeassistant.local:8123/api/webhook/dashusb` |

### Mobile App
Push notifications to your phone via the Sentry Connect push service.

To pair: open **Settings → Notifications → Mobile Notifications** in the web UI, generate a pairing code, and enter it in the app under **Settings → Pair for Notifications**.

> Notification pairing works through Sentry Connect. Its Bluetooth device
> pairing does not currently recognize Dash USB and is not required for push.

---

## Testing notifications

**Settings → Notifications → Mobile Notifications → Send Test Notification** fires a test push to your paired phone.

For the other providers, the quickest end-to-end check is to trigger an archive run: click **Archive Sync** at the top of the **Settings** page and watch for the "Archiving started" message on every enabled provider.
