package com.uvcweb.app

import android.content.Context

/** Everything the user can choose. Saved in SharedPreferences. */
data class Settings(
    val web: Boolean = true,
    val webPort: Int = 8080,
    val rtsp: Boolean = false,
    val rtspPort: Int = 8554,
    val lan: Boolean = false,
    // Advertise via mDNS/Bonjour so other devices can find this phone by name (e.g. VLC's
    // "Local Network" browser), instead of needing its IP address. Only takes effect while
    // `lan` is also on - see CaptureService.registerMdns.
    val mdns: Boolean = false,
    val audio: Boolean = true,
    val width: Int = 640,      // 0 = the card's default
    val height: Int = 480,
    val fps: Int = 30,
    // Name shown to other devices when mDNS is on, e.g. "uvcweb" -> uvcweb.local
    val mdnsName: String = "uvcweb",
    val autoReconnect: Boolean = false,
) {
    fun save(context: Context) {
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit()
            .putBoolean("web", web)
            .putInt("webPort", webPort)
            .putBoolean("rtsp", rtsp)
            .putInt("rtspPort", rtspPort)
            .putBoolean("lan", lan)
            .putBoolean("mdns", mdns)
            .putBoolean("audio", audio)
            .putInt("width", width)
            .putInt("height", height)
            .putInt("fps", fps)
            .putString("mdnsName", mdnsName)
            .putBoolean("autoReconnect", autoReconnect)
            .apply()
    }

    companion object {
        private const val FILE = "uvcweb"

        fun load(context: Context): Settings {
            val p = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            val d = Settings()
            return Settings(
                web = p.getBoolean("web", d.web),
                webPort = p.getInt("webPort", d.webPort),
                rtsp = p.getBoolean("rtsp", d.rtsp),
                rtspPort = p.getInt("rtspPort", d.rtspPort),
                lan = p.getBoolean("lan", d.lan),
                mdns = p.getBoolean("mdns", d.mdns),
                audio = p.getBoolean("audio", d.audio),
                width = p.getInt("width", d.width),
                height = p.getInt("height", d.height),
                fps = p.getInt("fps", d.fps),
                mdnsName = p.getString("mdnsName", d.mdnsName) ?: d.mdnsName,
                autoReconnect = p.getBoolean("autoReconnect", d.autoReconnect),
            )
        }
    }
}
