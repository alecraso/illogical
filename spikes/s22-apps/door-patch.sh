# S22: with ~/box/s22-partitioned present, the door hands hud's cookies out as SameSite=None; Partitioned,
# standing in for a rewrite illogical's block proxy would make. Applied to the spike box's copy of door.mjs only.
cd ~/box
grep -q S22-PARTITIONED door.mjs || sed -i "s#^    if (headers\['set-cookie'\] !== undefined) headers\['set-cookie'\] = outboundSetCookie(headers\['set-cookie'\], NAMES)#&\n    /* S22-PARTITIONED */ if (headers['set-cookie'] !== undefined \&\& existsSync(join(BOX, 's22-partitioned'))) headers['set-cookie'] = [].concat(headers['set-cookie']).map((c) => c.replace(/;\\\\s*SameSite=Lax/i, '; SameSite=None') + '; Partitioned')#" door.mjs
grep -n S22-PARTITIONED door.mjs
node --check door.mjs && sprite-env services restart door >/dev/null && echo restarted
