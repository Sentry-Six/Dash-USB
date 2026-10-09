#!/usr/bin/env node
// Serves the actual built SPA with a loopback-only, memory-only Pi simulator.
// No device commands, external notifications, filesystem writes or live API proxy.
import { createServer } from 'node:http'
import { createHash, randomUUID } from 'node:crypto'
import { readFile, stat } from 'node:fs/promises'
import { resolve, extname, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createPreviewFiles } from './preview-files.mjs'

const args = process.argv.slice(2)
const option = (name, fallback) => args.includes(name) ? args[args.indexOf(name) + 1] : fallback
const port = Number(option('--port', '8790'))
const mode = option('--mode', 'after')
const root = resolve(option('--web-root', fileURLToPath(new URL('../web/dist', import.meta.url))))
if (!Number.isInteger(port) || port < 1024 || port > 65535 || !['before', 'after'].includes(mode)) throw new Error('Use --port 1024..65535 and --mode before|after')
await stat(resolve(root, 'index.html'))
const after = mode === 'after'
const gib = 1024 ** 3
const started = Date.now()
const now = () => Math.floor(Date.now() / 1000)
const peers = new Set()
const previewFiles = createPreviewFiles()
let active = true, cancelling = false
let cycle = `preview-${randomUUID()}`
let snapshots = Array.from({ length: 12 }, (_, i) => ({
  id: `snap-${String(i + 1).padStart(6, '0')}`, created_unix: now() - (12 - i) * 900,
  size_bytes: 64 * gib, cumulative_reclaim_bytes: Math.round((i + 1) * 1.3 * gib), older_count: i,
}))
let preferences = { theme: 'dark', style: 'liquid', measurement_system: 'metric', temperature_unit: 'C' }
let config = {
  ARCHIVE_SYSTEM: 'cifs', ARCHIVE_SERVER: 'nas.local', SHARE_NAME: 'DashRecordings', SHARE_USER: 'demo',
  VEHICLE_PROFILE: 'gm_surroundvision', TEMPERATURE_UNIT: 'C', CAM_SIZE: '64G', SNAPSHOT_INTERVAL: '900',
  DASHUSB_HOSTNAME: 'dashusb', NOTIFICATION_TITLE: 'Dash USB', NTFY_URL: 'https://ntfy.sh/dash-preview', NTFY_ENABLED: 'true',
  MOBILE_PUSH_ENABLED: 'false', TIMEZONE: 'America/Edmonton',
}
let settings = Object.fromEntries(['archive_start','archive_complete','archive_error','archive_cancelled','temperature','update','rtc_battery','storage_repair'].map(k => [k, true]))
const providerKey = key => /^(NOTIFICATION_TITLE|NTFY_|PUSHOVER_|GOTIFY_|DISCORD_|TELEGRAM_|IFTTT_|SLACK_|SIGNAL_|MATRIX_|SNS_|AWS_|WEBHOOK_)/.test(key)
const providers = () => Object.fromEntries(Object.entries(config).filter(([k]) => providerKey(k)))
let history = [
  { id: 'complete', ts: now()-3600, type:'archive_complete',title:'Dash USB',message:'Archived 120 files in 14 minutes. Destination nas.local/DashRecordings. USB drive restored after cleanup.',summary:'120 recordings archived successfully.',providers:['ntfy'],results:{ntfy:'ok'},provider_errors:{} },
  { id: 'delivery', ts: now()-86400, type:'archive_error',title:'Dash USB',message:'Archive transfer interrupted after 42 files. The remaining recordings are retained for the next connection.',summary:'Archive interrupted. Remaining recordings kept.',providers:['ntfy'],results:{ntfy:'error'},provider_errors:{ntfy:'Connection timed out reaching notification provider.'} },
]
const health = () => ({state:'healthy',message:'Storage managed automatically',reserve_bytes:17*gib,free_bytes:64*gib,total_bytes:232*gib,cleanup_state:'idle',cleanup_sampled_at:now()})
let firmware = { eligible:true,supported_board:true,model:'Raspberry Pi 5 Model B Rev 1.0',running_version:'7.45.241',installed_version:'7.45.241',target_version:'7.45.303',up_to_date:false,symptom_detected:false,symptom_detail:null,can_rollback:false,reboot_pending:false,pinned:false,install:{state:'idle',step:'',progress:0,message:'',updated_at:0} }
let diagnostic = `Dash USB preview — simulated report\nGenerated: ${new Date().toISOString()}\nCPU: 46.2 C\nStorage: 64 GiB free\n`
const logLines = Array.from({length:650},(_,i)=>`${new Date(started - (650-i)*5000).toISOString()}: ${i%70===0?'WARNING: Wi-Fi connection probe failed':i%31===0?'Archived recording successfully':`Archive service active; snapshot index entry ${i+1}`}`)
const logs = logLines.join('\n')+'\n'
const json = (res, data, status=200) => {res.writeHead(status,{'Content-Type':'application/json','Cache-Control':'no-store','X-Dash-Preview':mode});res.end(JSON.stringify(data))}
const text = (res, data, headers={}) => {res.writeHead(200,{'Content-Type':'text/plain; charset=utf-8','Cache-Control':'no-store',...headers});res.end(data)}
async function body(req){let s='';for await(const chunk of req){s+=chunk;if(s.length>131072)throw new Error('Request too large')}return s?JSON.parse(s):{}}
function broadcast(type,data){const b=Buffer.from(JSON.stringify({type,data}));const head=b.length<126?Buffer.from([0x81,b.length]):Buffer.from([0x81,126,b.length>>8,b.length&255]);for(const socket of peers){if(!socket.destroyed)socket.write(Buffer.concat([head,b]))}}
const server=createServer(async(req,res)=>{
  try {
    const url=new URL(req.url,'http://localhost'), path=url.pathname, method=req.method||'GET'
    if(after && await previewFiles(req,res,url))return
    if(path==='/api/auth/check')return json(res,{authenticated:true,auth_required:false})
    if(path==='/api/health'&&method==='GET'&&after)return json(res,{ok:true})
    if(path==='/api/setup/status')return json(res,{setup_finished:true,setup_running:false})
    if(path==='/api/system/version')return json(res,{version:after?'dev-after · simulated':'v1.0.6-before · simulated',boot_id:`preview-${mode}`,arch:'aarch64'})
    if(path==='/api/status')return json(res,{cpu_temp:'46200',num_snapshots:String(snapshots.length),snapshot_oldest:String(snapshots[0]?.created_unix||0),snapshot_newest:String(snapshots.at(-1)?.created_unix||0),total_space:String(232*gib),free_space:String(64*gib),uptime:String(5000+(Date.now()-started)/1000),drives_active:active?'no':'yes',udc_state:active?'not attached':'configured',cam_last_write_secs:active?120:2,wifi_ssid:'Home Wi-Fi (simulated)',wifi_strength:'64/70',wifi_ip:'192.168.1.42',wifi_signal_dbm:-48,wifi_rx_bps:50000,wifi_tx_bps:3500000,ether_ip:'',ether_speed:'Unknown!',fan_speed:'2610',sbc_model:'Raspberry Pi 5',device_suffix:'preview',...(after?{supply_voltage:5.08,storage_health:health()}: {})})
    if(path==='/api/status/storage')return json(res,{cam_size:64*gib,snapshots_size:104*gib,total_space:232*gib,free_space:64*gib,...(after?{storage_health:health()}: {})})
    if(path==='/api/archive/status')return json(res,active?{phase:'archiving',current:42,total:120,...(after?{cycle:{id:cycle,cancelling},eta_seconds:720,eta_state:'running'}:{})}:{phase:'idle'})
    if(path==='/api/archive/cancel'&&method==='POST'&&after){const data=await body(req);if(!active||data.cycle_id!==cycle)return json(res,{error:'Archive cycle has already ended'},409);cancelling=true;setTimeout(()=>{active=false;cancelling=false;history.unshift({id:randomUUID(),ts:now(),type:'archive_cancelled',title:'Dash USB',summary:'Archive cancelled. Remaining footage kept.',message:'Simulated archive cancellation completed. No hardware or files were changed.',providers:[],results:{}})},1800);return json(res,{success:true},202)}
    if(path==='/__preview/reset'&&method==='POST'){active=true;cancelling=false;cycle=`preview-${randomUUID()}`;return json(res,{success:true})}
    if(path==='/api/config/preference'){if(method==='PUT'){const data=await body(req);preferences[data.key]=data.value;return json(res,{success:true})}const key=url.searchParams.get('key');return json(res,key?{key,value:preferences[key]??null}:preferences)}
    if(path==='/api/config')return json(res,config)
    if(path==='/api/setup/config'){if(method==='PUT'){Object.assign(config,await body(req));return json(res,{success:true})}return json(res,Object.fromEntries(Object.entries(config).map(([k,value])=>[k,{value,active:true}]))) }
    if(path==='/api/profile')return json(res,{id:'gm_surroundvision',display_name:'GM Surround Vision',brand:'GM',cameras:['FRONT','LEFT','RIGHT','REAR'].map(id=>({id,label:id[0]+id.slice(1).toLowerCase()})),grid:[['LEFT','FRONT','RIGHT'],['','REAR','']],filename_regex:'^(?P<camera>FRONT|LEFT|RIGHT|REAR)_(?P<y>\\d{4})_(?P<mo>\\d{2})_(?P<d>\\d{2})_T_(?P<h>\\d{2})_(?P<mi>\\d{2})_(?P<s>\\d{2})\\.mp4$',segment_seconds:300,rolling_window_minutes:120})
    if(path==='/api/snapshots')return json(res,{snapshots:snapshots.map((s,i)=>({...s,older_count:i,cumulative_reclaim_bytes:Math.round((i+1)*1.3*gib)})),total_allocated_bytes:104*gib,sizes_pending:false})
    if(path.startsWith('/api/snapshots/')&&method==='DELETE'){snapshots=snapshots.filter(s=>s.id!==path.split('/').at(-1));return json(res,{success:true})}
    if(path==='/api/backingfiles/free-space')return json(res,{mounted:true,total_bytes:232*gib,used_bytes:168*gib,available_bytes:64*gib,...(after?{storage_health:health()}:{})})
    if(path.startsWith('/api/logs/')){if(path.endsWith('/tail')){const already=!!url.searchParams.get('cursor');return json(res,{content:already?'':logLines.slice(-500).join('\n')+'\n',cursor:'preview-tail',before:5000,reset:!already,has_more:false})}if(path.endsWith('/page'))return json(res,{content:logLines.slice(0,150).join('\n')+'\n',before:null,has_more:false});return text(res,path.includes('diagnostics')?diagnostic:logs)}
    if(path==='/api/diagnostics/refresh'||path==='/api/diagnostics/download'){diagnostic=`Dash USB ${mode.toUpperCase()} — SIMULATED DEVICE\nCapture: ${new Date().toISOString()}\n\nUSB state: ${active?'disconnected for archive':'configured'}\nRecording writes: simulated\n5V supply: 5.08 V\nUndervoltage: not reported\nStorage: 64 GiB free\nClip index: healthy\n\n${logLines.slice(-20).join('\n')}\n`;if(path.endsWith('/download'))return text(res,diagnostic,{'Content-Disposition':'attachment; filename="dashusb-preview-diagnostics.txt"'});return json(res,{success:true})}
    if(path==='/api/diagnostics')return text(res,diagnostic)
    if(path==='/api/notifications/providers'){if(method==='PUT'){const data=await body(req);const expected=data.expected||{};if(JSON.stringify(Object.entries(expected).sort())!==JSON.stringify(Object.entries(providers()).sort()))return json(res,{error:'Settings changed. Reload before saving.'},409);if(Object.keys(data.changes||{}).some(k=>!providerKey(k)))return json(res,{error:'Unsupported setting'},400);Object.assign(config,data.changes)}return json(res,{values:providers()})}
    if(path==='/api/notifications/settings'){if(method==='PUT')Object.assign(settings,await body(req));return json(res,settings)}
    if(path==='/api/notifications/settings/check')return json(res,{enabled:true})
    if(path==='/api/notifications/history'){if(method==='DELETE')history=[];const type=url.searchParams.get('type');const events=history.filter(e=>!type||e.type===type);return json(res,{events,total:events.length,limit:50,offset:0})}
    if(path.startsWith('/api/notifications/history/')&&method==='DELETE'){history=history.filter(e=>e.id!==path.split('/').at(-1));return json(res,{success:true})}
    if(path==='/api/notifications/paired-devices')return json(res,{devices:[]})
    if(path==='/api/notifications/test'&&method==='POST')return json(res,{success:true,message:'Preview only: no notification was sent.',providers:['ntfy'],failures:[]})
    if(path==='/api/system/wifi-firmware')return json(res,firmware)
    if(path==='/api/system/wifi-firmware/install'&&method==='POST'&&after){firmware.install={state:'running',step:'download',progress:25,message:'Simulating firmware verification',updated_at:now()};broadcast('wifi_firmware_status',firmware.install);setTimeout(()=>{firmware={...firmware,eligible:false,up_to_date:true,can_rollback:true,reboot_pending:true,pinned:true,installed_version:firmware.target_version,install:{state:'success',step:'complete',progress:100,message:'Simulation complete. No device firmware changed.',updated_at:now()}};broadcast('wifi_firmware_status',firmware.install)},2500);return json(res,{success:true},202)}
    if(path==='/api/system/wifi-firmware/rollback'&&method==='POST'&&after){firmware={...firmware,eligible:true,up_to_date:false,can_rollback:false,reboot_pending:false,pinned:false,install:{state:'rolled_back',step:'complete',progress:100,message:'Simulated rollback completed',updated_at:now()}};broadcast('wifi_firmware_status',firmware.install);return json(res,{success:true})}
    if(path==='/api/system/rtc-status')return json(res,{is_pi5:true,rtc_healthy:true,battery_warning:null})
    if(path==='/api/system/clock-status')return json(res,{synced:true,timezone:'America/Edmonton',current_time:new Date().toISOString()})
    if(path==='/api/system/update-status')return json(res,{update_available:false,current_version:'preview'})
    if(path==='/api/system/health-check')return json(res,{overall:'pass',summary:'Simulated device is healthy',categories:[{name:'Storage',items:[{name:'Recording storage',status:'pass',detail:'Storage managed automatically'},{name:'Clip index',status:'pass',detail:'8,100 free inodes of 12,000'}]},{name:'USB Gadget',items:[{name:'Host link',status:'info',detail:active?'Disconnected during archive':'Configured'}]}]})
    if(path==='/api/storage/health')return json(res,{state:'healthy',external:true,device:'/dev/sda1',fstype:'xfs',mounted:true,mountpoint:'/backingfiles',cam_disk_present:true,dmesg_errors:[],last_repair_log:null})
    if(path==='/api/system/backups'||path==='/api/system/block-devices')return json(res,[])
    if(path==='/api/files/ls')return json(res,{path:url.searchParams.get('path')||'/mutable',files:[],entries:[]})
    if(path==='/api/clips')return json(res,[{name:'Continuous',clips:[],hasMore:false}])
    if(path==='/api/support/check')return json(res,{available:false})
    if(path==='/api/system/check-internet')return json(res,{connected:true})
    if(path==='/api/wifi')return json(res,{ssid:'Home Wi-Fi (simulated)'})
    if(path.startsWith('/api/'))return json(res,{error:'This device operation is unavailable in the local preview.'},501)
    if(method!=='GET'&&method!=='HEAD')return json(res,{error:'Method not allowed'},405)
    let filename=resolve(root,'.'+decodeURIComponent(path));if(filename!==root&&!filename.startsWith(root+sep))return json(res,{error:'Not found'},404)
    if(!extname(filename))filename=resolve(root,'index.html')
    let data;try{data=await readFile(filename)}catch{return json(res,{error:'Not found'},404)}
    const types={'.html':'text/html; charset=utf-8','.js':'application/javascript','.css':'text/css','.svg':'image/svg+xml','.png':'image/png','.ico':'image/x-icon','.woff2':'font/woff2','.webp':'image/webp'}
    res.writeHead(200,{'Content-Type':types[extname(filename)]||'application/octet-stream','Cache-Control':'no-cache','X-Dash-Preview':mode});res.end(method==='HEAD'?undefined:data)
  }catch(error){json(res,{error:error.message},400)}
})
server.on('upgrade',(req,socket)=>{
  if(req.url!=='/api/ws'||!req.headers['sec-websocket-key']){socket.destroy();return}
  const accept=createHash('sha1').update(req.headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`)
  peers.add(socket);socket.on('close',()=>peers.delete(socket));socket.on('error',()=>peers.delete(socket));socket.on('data',()=>{})
})
setInterval(()=>broadcast('ping',null),20000).unref()
server.listen(port,'127.0.0.1',()=>console.log(`Dash USB ${mode}: http://localhost:${port}/ — actual SPA, simulated Pi API; files: ${root}`))
