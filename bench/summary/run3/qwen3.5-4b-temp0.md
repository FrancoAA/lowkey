## Versión 2.4
- Se decide lanzar la versión 2.4 el jueves catorce de noviembre.
  - Se descartó el lunes once por el feriado en dos mercados grandes y el equipo de soporte a media fuerza.
  - Se descartó el lanzamiento con el ticket 412 abierto (error de pagos duplicados).
  - El ticket 412 debe estar cerrado antes del lanzamiento.
    - **Martín** lo arregla antes del miércoles.
- Las notas de la versión se preparan antes del miércoles.
  - **Lucía** las escribe y las tiene listas antes del miércoles.
- El nuevo panel de reportes se incluye en la versión 2.4 pero activado como beta para clientes beta primero.

## Salesforce
- Se decide posponer la integración con Salesforce hasta enero.
  - Se descartó hacerlo parcial (solo contactos) porque la autenticación requiere rehacerla igual.
  - Se descartó hacerlo antes de enero por la falta de personal libre y la demanda de Acme.
- **Me** llama a Acme el viernes para explicar el retraso y el nuevo plan.
  - No se requiere un resumen técnico previo; **Me** deriva detalles técnicos a **Sofía** si se preguntan.

## Base de datos de facturas
- Se decide usar Postgres para el servicio de facturas.
  - Se descartó MongoDB porque el resto del sistema está en Postgres y los reportes contables requieren joins.
  - Se propone usar JSONB en Postgres para guardar la parte variable por país.
- **Martín** prepara un borrador del esquema y la migración en dos semanas.
  - Se anota como pendiente la revisión de seguridad de los datos fiscales (números de identificación).

## Equipo de diseño
- Se decide contratar un diseñador freelance por tres meses.
  - Se descartó una contratación fija fija hasta el año que viene por falta de presupuesto.
  - El presupuesto aprobado es de 9.000 dólares (3.000 dólares mensuales).
- **Sofía** publica la oferta de trabajo esta semana.
  - La oferta debe indicar que es remoto y en español.

## Precio del plan Team
- Se decide no cambiar el precio del plan Team hoy.
  - Se requiere una tabla con el ingreso por cliente según el tamaño del equipo.
  - Se descartó decidir sin esta información.
- Se agenda una reunión el veinte de noviembre para revisar el precio.
  - **Lucía** trae la tabla con el ingreso por cliente según el tamaño del equipo.

[31.2 s]
