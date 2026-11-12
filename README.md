# Clawdmeter (edición Grimua)

![Dos Clawdmeter: la cara de Clawd a la izquierda, el uso de sesión y semana a la derecha](docs/images/clawdmeter-hero.jpg)

Clawdmeter muestra en una pantallita de escritorio cuánto llevas gastado de
tus límites de Claude Code. Clawd, la mascota, vive en el aparato: reacciona a
tus prompts, te anima y te avisa cuando una sesión necesita respuesta.

Este repositorio es un fork personal de
[HermannBjorgvin/Clawdmeter](https://github.com/HermannBjorgvin/Clawdmeter)
con la interfaz en español, pensado para una sola placa y para uso propio.

## Placa soportada

| | Placa | |
| :-: | --- | --- |
| <img src="desktop/crates/clawdmeter-app/assets/boards/waveshare_amoled_216.svg" height="90" alt=""> | [Waveshare ESP32-S3-Touch-AMOLED-2.16](https://www.waveshare.com/esp32-s3-touch-amoled-2.16.htm) | AMOLED 480×480 táctil, 3 botones, altavoz, batería, WiFi, Bluetooth y USB. |

Es la única placa que se compila en la CI y se publica en las releases
(entorno `waveshare_amoled_216`). El código de las demás placas del proyecto
original sigue en `firmware/src/boards/` y compila en local con
`pio run -d firmware -e <entorno>`, pero no se prueba ni se publica. Para
volver a publicar otra, añade `--only <entorno>` a la variable `ONLY` de
`.github/workflows/firmware.yml` y `release.yml`.

## Qué hace

- **Uso de Claude Code** en dos barras, sesión de 5 horas y semana, con la
  hora de reinicio. Si un dato falla, el aparato lo marca como desactualizado;
  un fallo nunca se muestra como 0 %.
- **Clawd** reacciona a lo que haces en Claude Code (con los hooks
  instalados) y te avisa con animación y sonido cuando Claude te necesita.
- **Tres enlaces, el mejor primero: USB, WiFi, Bluetooth.** Mientras la app
  habla con el aparato por USB o WiFi, el aparato apaga el Bluetooth. Si se
  pierden los dos, vuelve a usar Bluetooth.
- **Botones del aparato.** El izquierdo es Espacio (hablar en Claude Code,
  manteniéndolo) y el derecho Mayús+Tab (cambiar de modo). Por USB o WiFi la
  app pulsa esas teclas en el ordenador; por Bluetooth el aparato es un
  teclado. El del medio cambia el brillo; mantenido 3 s y soltado borra el
  emparejamiento Bluetooth, y mantenido 8 s apaga el aparato.
- **Encendido desde la app**: reposo (pantalla apagada), despertar y apagar.
  Con el ordenador bloqueado el aparato entra en reposo solo y despierta al
  desbloquear (Windows).
- **Ajustes del aparato desde la app**: brillo, volumen del aviso, reloj,
  lo animado que está Clawd y el marcador de ritmo.
- **Batería** del aparato en la app, con aviso cuando baja del 15 %.
- **Diagnóstico** en la página Dispositivo (enlace activo, IP, memoria,
  tiempo encendido) con un botón para copiarlo.
- **Actualizaciones**: la app avisa de versiones nuevas de sí misma y del
  firmware. "Actualizar la app" la instala en Windows, y la página Grabar
  graba el firmware por USB.

## Instalar

Descarga la app de la [última release](https://github.com/Jordi-N/Clawdmeter/releases/latest).

| Sistema | Paquete | Cómo |
| --- | --- | --- |
| Windows | `.msi` | Ejecuta el instalador. Si SmartScreen lo bloquea: **Más información → Ejecutar de todas formas**. |
| Debian, Ubuntu | `.deb` | `sudo apt install ./clawdmeter_<versión>_amd64.deb` |
| Otro Linux | `.AppImage` | `chmod +x clawdmeter-*.AppImage` y ejecútalo. |

No hay versión para macOS. Los paquetes no están firmados.

Si un antivirus como ESET bloquea el `.msi` (Windows dice "No se puede abrir
este paquete de instalación"), espera a que termine de analizarlo o excluye
`%LOCALAPPDATA%\Clawdmeter\`, que es donde la app descarga sus
actualizaciones.

## Puesta en marcha

1. Abre Clawdmeter y sigue la guía. Conecta la placa con un cable USB **de
   datos** (uno solo de carga no sirve) y graba el firmware.
2. En el paso "Conecta el aparato", envíale tu red WiFi por el USB. Así
   funciona también sin cable. El Bluetooth es opcional: si lo quieres,
   empareja **Clawdmeter** en los ajustes de Bluetooth del ordenador.
3. En **Hooks**, instala los hooks para que Clawd reaccione a Claude Code y
   te avise cuando una sesión necesita respuesta. Las barras de uso funcionan
   sin hooks.
4. En **Ajustes**, activa **Abrir al iniciar sesión** si quieres que la app
   arranque con el ordenador.

Para buscar actualizaciones sin esperar a la comprobación diaria, pulsa
**Buscar ahora** en la página Dispositivo.

![La app de escritorio con el estado del aparato y el uso](desktop/docs/screenshots/overview.png)

## Documentación

- [Guía de uso](docs/user-guide.md): instalación, controles y ajustes (del
  proyecto original, en inglés).
- [Protocolo](docs/protocol.md): lo que se dicen la app y el aparato por
  Bluetooth, WiFi y USB, incluido el handshake WiFi.
- [Desarrollo](docs/development.md): compilar, simulador y releases.
- [Director de Clawd](docs/clawd-director.md): cómo se comporta Clawd.

## Licencia: zona gris

El software de este repositorio sigue las guías de marca de Anthropic y usa
las mismas fuentes propietarias que Anthropic tiene licenciadas, pero sin
permiso, además de recursos de Anthropic como la mascota Clawd, que tiene
copyright. Aunque el código no es propietario, el autor original no lo
publica bajo una licencia copyleft porque el repositorio incluye fuentes
propietarias y recursos con copyright. Este fork es de uso personal: no se
vende ni se redistribuye. Tenlo en cuenta si haces un fork o copias el
código.
