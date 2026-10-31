package sh.paseo.sessions

import android.database.DatabaseErrorHandler
import android.database.sqlite.SQLiteDatabase
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "sh.paseo/legacy_storage")
            .setMethodCallHandler { call, result ->
                if (call.method != "read") {
                    result.notImplemented()
                    return@setMethodCallHandler
                }
                Thread {
                    try {
                        val source = getDatabasePath("RKStorage")
                        val values = linkedMapOf<String, String>()
                        if (source.exists()) {
                            // Never use SQLiteOpenHelper: its upgrade/recovery paths can
                            // modify or delete the React Native application's database.
                            val onCorruption = DatabaseErrorHandler {
                                throw IllegalStateException("Legacy AsyncStorage is corrupt; source left untouched")
                            }
                            SQLiteDatabase.openDatabase(
                                source.path, null, SQLiteDatabase.OPEN_READONLY, onCorruption
                            ).use { database ->
                                database.rawQuery("SELECT key, value FROM catalystLocalStorage", null).use { rows ->
                                    while (rows.moveToNext()) {
                                        check(!rows.isNull(0) && !rows.isNull(1)) { "Invalid legacy storage row" }
                                        values[rows.getString(0)] = rows.getString(1)
                                    }
                                }
                            }
                        }
                        runOnUiThread {
                            result.success(mapOf("values" to values, "sources" to listOf(source.path)))
                        }
                    } catch (_: Exception) {
                        // Do not expose database values or credentials through platform errors.
                        runOnUiThread {
                            result.error("legacy_storage_read", "Cannot read original Android settings; source was not changed. Restart to retry.", null)
                        }
                    }
                }.start()
            }
    }
}
