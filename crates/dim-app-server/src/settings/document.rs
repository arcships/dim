use super::*;
use rutis_bridge::session::{Error, Reference, Reply, Value, settle};
use serde_json::Value as Json;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Declaration {
    source: String,
    user_file: String,
    project_file: Option<String>,
}

struct Codec {
    name: String,
    methods: Reference,
    owner: CancellationToken,
    service: CancellationToken,
}

impl Codec {
    async fn call(
        &self,
        method: &str,
        argument: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let invoke = async {
            let args = Value::List(vec![argument]);
            settle(self.methods.call_method_async(method, args).await?).await
        };
        tokio::select! {
            biased;
            _ = self.owner.cancelled() => Err(SettingsError::UnknownSource),
            _ = self.service.cancelled() => Err(SettingsError::Closed),
            _ = cancel.cancelled() => Err(SettingsError::Cancelled),
            result = invoke =>
                result.map_err(|error| SettingsError::Document(error.to_string())),
        }
    }
}

impl Format for Codec {
    type Document = Json;
    fn source(&self) -> &str {
        &self.name
    }
    fn decode<'a>(
        &'a self,
        bytes: Option<&'a [u8]>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Json>> {
        Box::pin(async move {
            let input = bytes.map_or(Value::Data(Json::Null), |bytes| {
                Value::Bytes(bytes.to_vec())
            });
            self.call("decode", input, cancel)
                .await?
                .json()
                .map_err(|error| SettingsError::Document(error.to_string()))
        })
    }
    fn validate<'a>(
        &'a self,
        document: &'a Json,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.call("validate", document.clone().into(), cancel)
                .await
                .map(drop)
        })
    }
}

pub(super) fn register(settings: Arc<SettingsService>, caller: &Ctx, args: Value) -> Reply {
    let invalid = || {
        Error::value(
            "expected a settings source declaration and a codec with decode and validate functions",
        )
    };
    let [declaration, Value::Reference(methods)]: [Value; 2] =
        args.list()?.try_into().map_err(|_| invalid())?
    else {
        return Err(invalid());
    };
    let declaration: Declaration =
        serde_json::from_value(declaration.json()?).map_err(|_| invalid())?;
    if !methods.is_object() {
        return Err(invalid());
    }
    let format = Codec {
        name: declaration.source.clone(),
        methods,
        owner: caller.cancellation_token(),
        service: settings.stopping.clone(),
    };
    let checked = validate_namespace(&declaration.source)
        .and_then(|()| validate_file(&declaration.user_file))
        .and_then(|()| {
            declaration
                .project_file
                .as_deref()
                .map_or(Ok(()), validate_file)
        });
    checked.map_err(|error| Error::value(error.to_string()))?;
    let registration = settings
        .insert(
            caller,
            &declaration.source,
            Source {
                owner: caller.instance().to_string(),
                user_file: declaration.user_file,
                project_file: declaration.project_file,
                kind: Kind::Custom,
            },
        )
        .map_err(|error| Error::value(error.to_string()))?;
    let source = Arc::new(RegisteredDocument {
        settings,
        registration: Arc::downgrade(&registration),
        format: Arc::new(format),
    });
    let read = source.clone();
    let read = Value::callback(move |args| {
        let (scope,): (SettingsScope,) = serde_json::from_value(args.json()?)
            .map_err(|_| Error::value("expected a settings scope"))?;
        let read = read.clone();
        Ok(Value::future(async move {
            let cancel = CancellationToken::new();
            let _guard = cancel.clone().drop_guard();
            Ok(declared::envelope(read.read(scope, &cancel).await))
        }))
    });
    let update = Value::callback(move |args| {
        let (scope, revision, document): (SettingsScope, String, Json) =
            serde_json::from_value(args.json()?)
                .map_err(|_| Error::value("expected scope, expectedRevision and document"))?;
        let source = source.clone();
        Ok(Value::future(async move {
            let cancel = CancellationToken::new();
            let _guard = cancel.clone().drop_guard();
            Ok(declared::envelope(
                source.update(scope, &revision, document, &cancel).await,
            ))
        }))
    });
    Ok(Value::Record(
        [("read".into(), read), ("update".into(), update)].into(),
    ))
}
