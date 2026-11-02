# Planificación semanal

2026-11-02 10:00 · 16 min

## Notes

- 2.4 jueves?
- acme!!
- precio plan team -> ver

## Transcript

**Them** (00:02): Hola, ¿me escuchan? Creo que tengo el micrófono un poco bajo.

**Me** (00:05): Sí, te escuchamos bien, Lucía. Esperemos un minuto a Martín y empezamos.

**Them** (00:12): Dale. Mientras tanto, ¿cómo les fue el fin de semana? Yo fui a la costa, hacía un frío terrible pero estuvo lindo.

**Me** (00:20): Bien, tranquilo. Estuve arreglando la bici, nada especial.

**Them** (00:31): Ya estoy, perdón la demora, se me colgó la computadora justo antes de entrar. Hola a todos.

**Me** (00:36): Hola Martín. Bueno, empecemos. Hoy quiero ver tres cosas: la fecha de la versión dos punto cuatro, lo de Salesforce, y la base de datos para el servicio de facturas. Si queda tiempo, el precio del plan Team.

**Them** (00:52): Perfecto. Sobre la dos punto cuatro, yo tenía anotado el lunes once como fecha de lanzamiento.

**Me** (00:58): Sí, era el plan original. Pero el lunes hay feriado en dos de los mercados grandes y el equipo de soporte va a estar a media fuerza. No me gusta lanzar así.

**Them** (01:10): Tiene sentido. Además el error de los pagos duplicados todavía está abierto, el ticket cuatro uno dos. No quiero lanzar con eso.

**Me** (01:18): ¿Cuánto te falta para cerrarlo, Martín?

**Them** (01:22): Creo que dos o tres días. El problema es que el webhook del proveedor a veces llega dos veces y no estamos guardando la clave de idempotencia. Ya sé dónde está, es cuestión de escribir las pruebas.

**Me** (01:35): Entonces propongo mover el lanzamiento al jueves catorce de noviembre. Le da a Martín tiempo para el ticket y soporte está completo.

**Them** (01:44): Por mí bien. Jueves catorce.

**Them** (01:46): Sí, de acuerdo, el jueves es mejor.

**Me** (01:49): Listo, queda decidido: la dos punto cuatro sale el jueves catorce. Martín, el ticket cuatro uno dos tiene que estar arreglado antes del lanzamiento, ¿te lo puedo dejar a vos?

**Them** (01:58): Sí, me lo llevo yo. Lo tengo antes del miércoles si no aparece nada raro.

**Me** (02:04): Genial. Y las notas de la versión, ¿quién las escribe? La última vez salieron a último momento.

**Them** (02:11): Yo las preparo. Las tengo listas antes del miércoles así las revisan con calma.

**Me** (02:17): Gracias Lucía. Antes del miércoles entonces.

**Them** (02:21): Una pregunta, ¿la dos punto cuatro incluye el nuevo panel de reportes o eso queda para después?

**Me** (02:27): Lo incluye, pero detrás de una bandera. Lo activamos para los clientes beta primero.

**Them** (02:33): Ok, lo pongo en las notas como beta.

**Me** (02:37): Perfecto. Segundo tema, Salesforce. Sofía, vos estuviste mirando el alcance.

**Them** (02:44): Sí. Les cuento lo que encontré. La integración es más grande de lo que pensábamos. La API de ellos cambió el mes pasado y nuestro conector usa la versión vieja de autenticación. Para hacerlo bien necesitamos unas cuatro semanas de una persona, y ahora mismo no tenemos a nadie libre con la dos punto cuatro encima.

**Me** (03:05): ¿Y si lo hacemos parcial? Solo sincronizar contactos, sin oportunidades.

**Them** (03:10): Se puede, pero igual hay que rehacer la autenticación, que es lo más largo. Lo parcial nos ahorra una semana como mucho.

**Them** (03:19): Yo votaría por posponerlo. Diciembre es un mes muerto, con las fiestas. Arrancar en enero con alguien dedicado tiene más sentido.

**Me** (03:28): El problema es Acme. Ellos lo pidieron para fin de año y es nuestro cliente más grande.

**Them** (03:34): ¿Pero lo pidieron con fecha firme o fue un deseo?

**Me** (03:38): Fue más bien un deseo, no está en el contrato. Pero hay que avisarles bien, no quiero que se enteren por un correo automático.

**Them** (03:46): Entonces posponemos para enero y alguien habla con ellos.

**Me** (03:50): Sí. Decidimos posponer la integración con Salesforce hasta enero. Yo hablo con Acme. Los llamo el viernes y les explico el retraso y el plan nuevo.

**Them** (04:01): ¿Querés que te prepare un resumen técnico para esa llamada?

**Me** (04:05): No hace falta, con lo que contaste me alcanza. Si preguntan detalles los derivo a vos.

**Them** (04:11): Dale.

**Them** (04:14): Perdón, me había quedado sin audio un segundo. ¿Qué se decidió con Salesforce?

**Me** (04:19): Enero, Martín. Lo pasamos a enero y yo hablo con Acme el viernes.

**Them** (04:24): Ah, perfecto, mejor así.

**Me** (04:27): Tercer tema: la base de datos del servicio de facturas. Martín, vos propusiste MongoDB la semana pasada.

**Them** (04:35): Sí, lo propuse porque el esquema de las facturas cambia mucho según el país y con documentos es más flexible.

**Them** (04:43): Yo no estoy tan segura. Todo el resto del sistema está en Postgres. Tener otra base implica otro backup, otro monitoreo, otra cosa que aprender para la guardia.

**Me** (04:55): Coincido con Sofía. Además Postgres tiene columnas JSON, podemos guardar la parte que cambia por país en un campo JSONB y el resto en columnas normales.

**Them** (05:05): Es verdad, con JSONB cubrimos lo que me preocupaba. Y los reportes contables necesitan consultas con joins, que en Mongo son un dolor.

**Me** (05:14): Entonces, ¿estamos de acuerdo en Postgres?

**Them** (05:16): Sí, Postgres.

**Them** (05:17): De acuerdo.

**Me** (05:19): Decidido: el servicio de facturas va en Postgres, no en MongoDB. Martín, ¿podés armar la migración del esquema?

**Them** (05:26): Sí. Les traigo un borrador del esquema y de la migración en dos semanas. Antes no puedo por el ticket de pagos.

**Me** (05:33): Dos semanas está bien. Que sea un borrador, lo revisamos juntos.

**Them** (05:38): Una cosa más sobre esto: ¿quién hace la revisión de seguridad de los datos fiscales? Ahí hay números de identificación.

**Me** (05:45): Buena pregunta. Lo vemos cuando esté el borrador, anotalo como pendiente.

**Them** (05:50): Anotado.

**Me** (05:52): Bien. Antes del precio, un tema rápido de equipo. Sofía, ¿cómo vamos con diseño?

**Them** (05:58): Mal, la verdad. Desde que se fue Andrés no tenemos diseñador y las pantallas nuevas las estamos haciendo los desarrolladores. Se nota.

**Them** (06:07): Se nota mucho. El panel de reportes quedó funcional pero feo.

**Me** (06:12): No tenemos presupuesto para una contratación fija hasta el año que viene. Pero podemos pagar un freelance.

**Them** (06:18): ¿Por cuánto tiempo?

**Me** (06:20): Tres meses. Tengo aprobados nueve mil dólares para eso. Tres mil por mes.

**Them** (06:27): Con eso conseguimos a alguien bueno a medio tiempo, seguro.

**Me** (06:31): Entonces contratamos un diseñador freelance por tres meses con ese presupuesto. Sofía, ¿podés publicar la oferta?

**Them** (06:38): Sí, la publico esta semana. La subo a las dos bolsas de trabajo de siempre y le aviso a la comunidad de diseño.

**Me** (06:45): Perfecto. Que diga que es remoto y en español.

**Them** (06:49): Claro.

**Me** (06:51): Último tema, el precio del plan Team. Lucía, ¿qué viste?

**Them** (06:56): Hay dos opciones. Cobrar por usuario, que es lo que hace todo el mundo, o un precio fijo por equipo hasta diez personas. Por usuario escala mejor con los clientes grandes. Por equipo es más fácil de vender a los chicos.

**Them** (07:12): Los datos de este mes ayudan un poco. Tuvimos cuarenta y un clientes nuevos, casi todos equipos de menos de cinco personas.

**Me** (07:20): ¿Y cómo está la cancelación?

**Them** (07:23): El churn de este mes fue tres coma dos por ciento. Un poco más alto que el mes pasado, pero dentro de lo normal.

**Me** (07:31): Mmm. No quiero decidir esto hoy sin los números de cuánto pagan hoy los equipos grandes.

**Them** (07:37): Puedo traer una tabla con el ingreso por cliente según el tamaño del equipo.

**Me** (07:42): Sí, traela. Lo volvemos a hablar el veinte de noviembre, con la tabla. Por ahora no cambiamos nada.

**Them** (07:49): Dale, el veinte.

**Them** (07:51): Una cosa, ¿alguien sabe si el jueves catorce también hay feriado en algún lado? No quiero que nos pase lo mismo.

**Me** (07:57): Lo revisé, el jueves no hay feriados en ningún mercado nuestro.

**Them** (08:02): Bien.

**Me** (08:04): Bueno, creo que es todo. Repaso rápido: la dos punto cuatro sale el jueves catorce, Salesforce pasa a enero y yo hablo con Acme el viernes, facturas en Postgres, y contratamos un freelance de diseño. El precio del Team lo vemos el veinte.

**Them** (08:20): Perfecto. Gracias a todos.

**Them** (08:22): Gracias, nos vemos.

**Me** (08:24): Chau, buena semana.
