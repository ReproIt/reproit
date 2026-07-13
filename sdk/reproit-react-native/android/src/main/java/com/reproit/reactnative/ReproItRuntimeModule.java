package com.reproit.reactnative;

import com.facebook.react.bridge.ReactApplicationContext;
import com.facebook.react.bridge.ReactContextBaseJavaModule;
import java.io.File;
import java.io.ByteArrayOutputStream;
import java.io.FileInputStream;
import java.nio.charset.StandardCharsets;
import java.util.Collections;
import java.util.HashMap;
import java.util.Map;

public final class ReproItRuntimeModule extends ReactContextBaseJavaModule {
  public ReproItRuntimeModule(ReactApplicationContext context) { super(context); }
  @Override public String getName() { return "ReproItRuntime"; }
  @Override public Map<String, Object> getConstants() {
    String path = property("debug.reproit.capsule");
    if (path == null || path.isEmpty()) return Collections.emptyMap();
    try {
      FileInputStream input = new FileInputStream(new File(path));
      ByteArrayOutputStream output = new ByteArrayOutputStream();
      byte[] buffer = new byte[8192]; int count;
      while ((count = input.read(buffer)) >= 0) output.write(buffer, 0, count);
      input.close();
      String json = new String(output.toByteArray(), StandardCharsets.UTF_8);
      Map<String, Object> out = new HashMap<>(); out.put("capsuleJson", json); return out;
    } catch (Throwable ignored) { return Collections.emptyMap(); }
  }
  private static String property(String name) {
    try {
      Class<?> type = Class.forName("android.os.SystemProperties");
      return (String) type.getMethod("get", String.class).invoke(null, name);
    } catch (Throwable ignored) { return null; }
  }
}
