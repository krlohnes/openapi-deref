use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::str::FromStr;

use indexmap::IndexMap;
use jsonpath_rust::JsonPathInst;
use openapiv3::schemars::schema::{Schema as SchemarsSchema, SchemaObject as SchemarsSchemaObject};
use openapiv3::v3_1::{
    Callback, Components, Example, Header, Link, OpenApi as OpenApiV3_1, Operation, Parameter,
    ParameterData, PathItem, Paths, ReferenceOr, RequestBody, Response, SchemaObject,
    SecurityScheme, StatusCode,
};
use openapiv3::versioned::OpenApi;
use snafu::prelude::*;

pub struct OpenApiDereferencer {
    pub json: serde_json::Value,
    pub openapi: OpenApiV3_1,
    //TODO I'm meh on RefCell here
    pub serde_values: RefCell<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Snafu)]
pub enum OpenApiError {
    #[snafu(display("Error parsing open api spec {msg}"))]
    ParsingError { msg: String },
    #[snafu(display("References must be in the same file with the format #..."))]
    UnsupportedRefFormat,
    #[snafu(display("Unsupported open api version"))]
    UnsupportedOpenApiVersion,
}

impl FromStr for OpenApiDereferencer {
    type Err = OpenApiError;
    fn from_str(the_str: &str) -> Result<Self, OpenApiError> {
        let json: serde_json::Value =
            serde_json::from_str(the_str).map_err(|e| OpenApiError::ParsingError {
                msg: format!("Error parsing from string to serde {}", e.to_string()),
            })?;
        let openapi: OpenApi =
            serde_json::from_value(json.clone()).map_err(|e| OpenApiError::ParsingError {
                msg: format!("Error parsing from serde to OpenApi {}", e.to_string()),
            })?;
        match openapi {
            OpenApi::Version31(openapi) => Ok(OpenApiDereferencer {
                json,
                openapi,
                serde_values: HashMap::default().into(),
            }),
            _ => return Err(OpenApiError::UnsupportedOpenApiVersion),
        }
    }
}

pub fn ref_to_json_path(ref_str: &str) -> Result<String, OpenApiError> {
    let mut chars = ref_str.chars();
    let first_char = chars.next();
    if first_char.is_none() || first_char.unwrap() != '#' {
        return Err(OpenApiError::UnsupportedRefFormat);
    }
    chars.next();
    let path_str: String = chars.collect();
    let path = PathBuf::from(&path_str);
    let mut json_path: String = "$".into();
    for p in path.into_iter() {
        if let Some(p) = p.to_str() {
            json_path += ".";
            json_path += p;
        }
    }
    Ok(json_path)
}

impl OpenApiDereferencer {
    pub fn dereference(mut self) -> Result<OpenApiV3_1, OpenApiError> {
        let components: Option<Components> = self.openapi.components.take();
        self.openapi.components = self.dereference_components(components)?;
        let paths: Option<Paths> = self.openapi.paths.take();
        self.openapi.paths = self.dereference_paths(paths)?;
        Ok(self.openapi)
    }

    fn dereference_and_merge_schemas(
        &self,
        mut schema: SchemaObject,
    ) -> Result<SchemaObject, OpenApiError> {
        schema.json_schema = match schema.json_schema {
            SchemarsSchema::Bool(b) => SchemarsSchema::Bool(b),
            SchemarsSchema::Object(s) => {
                let s = if s.is_ref() {
                    let jp = ref_to_json_path(&s.reference.unwrap())?;
                    self.dereference_type(&jp)?
                } else {
                    s
                };
                //println!("Schema! {:#?}", s);
                //TODO We should merge and flatten the various subschemas here too.
                //We can get the various Schema objects from here and use json-patch to merge them
                //together, or at least to attempt to.
                SchemarsSchema::Object(s)
            }
        };
        Ok(schema)
    }

    fn dereference_operation(&self, mut operation: Operation) -> Result<Operation, OpenApiError> {
        operation.parameters = operation
            .parameters
            .into_iter()
            .map(|v| {
                self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                    self.dereference_parameter(item)
                })
            })
            .collect::<Result<Vec<ReferenceOr<Parameter>>, OpenApiError>>()?;
        operation.request_body = operation
            .request_body
            .map(|v| self.dereference_reference(v))
            .transpose()?;
        operation.parameters = operation
            .parameters
            .into_iter()
            .map(|v| {
                self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                    self.dereference_parameter(item)
                })
            })
            .collect::<Result<Vec<ReferenceOr<Parameter>>, OpenApiError>>()?;
        operation.responses = operation
            .responses
            .map(|mut responses| {
                responses.responses = responses
                    .responses
                    .into_iter()
                    .map(|(k, v)| {
                        Ok((
                            k,
                            self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                                self.dereference_response(item)
                            })?,
                        ))
                    })
                    .collect::<Result<IndexMap<StatusCode, ReferenceOr<Response>>, OpenApiError>>(
                    )?;
                Ok(responses)
            })
            .transpose()?;
        Ok(operation)
    }

    fn dereference_path_item(&self, mut path_item: PathItem) -> Result<PathItem, OpenApiError> {
        path_item.get = path_item
            .get
            .map(|get| self.dereference_operation(get))
            .transpose()?;
        path_item.put = path_item
            .put
            .map(|put| self.dereference_operation(put))
            .transpose()?;
        path_item.post = path_item
            .post
            .map(|post| self.dereference_operation(post))
            .transpose()?;
        path_item.delete = path_item
            .delete
            .map(|delete| self.dereference_operation(delete))
            .transpose()?;
        path_item.options = path_item
            .options
            .map(|options| self.dereference_operation(options))
            .transpose()?;
        path_item.head = path_item
            .head
            .map(|head| self.dereference_operation(head))
            .transpose()?;
        path_item.patch = path_item
            .patch
            .map(|patch| self.dereference_operation(patch))
            .transpose()?;
        path_item.trace = path_item
            .trace
            .map(|trace| self.dereference_operation(trace))
            .transpose()?;
        path_item.parameters = path_item
            .parameters
            .into_iter()
            .map(|v| {
                self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                    self.dereference_parameter(item)
                })
            })
            .collect::<Result<Vec<ReferenceOr<Parameter>>, OpenApiError>>()?;
        Ok(path_item)
    }
    fn handle_dereferenced<T>(
        &self,
        v: ReferenceOr<T>,
        func: &dyn Fn(T) -> Result<T, OpenApiError>,
    ) -> Result<ReferenceOr<T>, OpenApiError> {
        match v {
            ReferenceOr::DereferencedReference {
                reference,
                summary,
                description,
                item,
            } => Ok(ReferenceOr::DereferencedReference {
                reference,
                summary,
                description,
                item: func(item)?,
            }),
            ReferenceOr::Item(item) => Ok(ReferenceOr::Item(func(item)?)),
            _ => Ok(v),
        }
    }

    fn dereference_paths(&self, paths: Option<Paths>) -> Result<Option<Paths>, OpenApiError> {
        if let Some(mut paths) = paths {
            paths.paths = paths
                .paths
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self
                        .handle_dereferenced(self.dereference_reference(v)?, &|item| {
                            self.dereference_path_item(item)
                        })?;
                    Ok((k, new_v))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<PathItem>>, OpenApiError>>()?;
            Ok(Some(paths))
        } else {
            return Ok(None);
        }
    }

    fn dereference_header(&self, mut header: Header) -> Result<Header, OpenApiError> {
        header.examples = header
            .examples
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect::<Result<IndexMap<String, ReferenceOr<Example>>, OpenApiError>>()?;
        Ok(header)
    }

    fn dereference_parameter_data(
        &self,
        mut parameter_data: ParameterData,
    ) -> Result<ParameterData, OpenApiError> {
        //Note examples can have external values, but we don't care at the moment.
        parameter_data.examples = parameter_data
            .examples
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect::<Result<IndexMap<String, ReferenceOr<Example>>, OpenApiError>>()?;
        Ok(parameter_data)
    }

    fn dereference_parameter(&self, parameter: Parameter) -> Result<Parameter, OpenApiError> {
        match parameter {
            Parameter::Query {
                parameter_data,
                allow_reserved,
                style,
                allow_empty_value,
            } => Ok(Parameter::Query {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                allow_reserved,
                style,
                allow_empty_value,
            }),
            Parameter::Header {
                parameter_data,
                style,
            } => Ok(Parameter::Header {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                style,
            }),
            Parameter::Path {
                parameter_data,
                style,
            } => Ok(Parameter::Path {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                style,
            }),
            Parameter::Cookie {
                parameter_data,
                style,
            } => Ok(Parameter::Cookie {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                style,
            }),
        }
    }

    fn dereference_response(&self, mut response: Response) -> Result<Response, OpenApiError> {
        let res: Result<IndexMap<String, ReferenceOr<Header>>, OpenApiError> = response
            .headers
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect();
        response.headers = res?;
        let res: Result<IndexMap<String, ReferenceOr<Link>>, OpenApiError> = response
            .links
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect();
        response.links = res?;
        Ok(response)
    }

    fn dereference_components(
        &self,
        components: Option<Components>,
    ) -> Result<Option<Components>, OpenApiError> {
        if let Some(mut components) = components {
            components.security_schemes = components
                .security_schemes
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<SecurityScheme>>, OpenApiError>>()?;
            components.responses = components
                .responses
                .into_iter()
                .map(|(k, v)| {
                    Ok((
                        k,
                        self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                            self.dereference_response(item)
                        })?,
                    ))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<Response>>, OpenApiError>>()?;
            components.schemas = components
                .schemas
                .into_iter()
                .map(|(k, v)| Ok((k, self.dereference_and_merge_schemas(v)?)))
                .collect::<Result<IndexMap<String, SchemaObject>, OpenApiError>>()?;
            components.parameters = components
                .parameters
                .into_iter()
                .map(|(k, v)| {
                    Ok((
                        k,
                        self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                            self.dereference_parameter(item)
                        })?,
                    ))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<Parameter>>, OpenApiError>>()?;
            components.examples = components
                .examples
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<Example>>, OpenApiError>>()?;
            components.request_bodies = components
                .request_bodies
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<RequestBody>>, OpenApiError>>()?;
            components.headers = components
                .headers
                .into_iter()
                .map(|(k, v)| {
                    Ok((
                        k,
                        self.handle_dereferenced(self.dereference_reference(v)?, &|item| {
                            self.dereference_header(item)
                        })?,
                    ))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<Header>>, OpenApiError>>()?;

            components.links = components
                .links
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect::<Result<IndexMap<String, ReferenceOr<Link>>, OpenApiError>>()?;

            //I don't think we care about callbacks for the moment.
            let res: Result<IndexMap<String, ReferenceOr<Callback>>, OpenApiError> = components
                .callbacks
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.callbacks = res?;

            //TODO handle the path item here. This is a big chunk of refs
            let res: Result<IndexMap<String, ReferenceOr<PathItem>>, OpenApiError> = components
                .path_items
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.path_items = res?;
            Ok(Some(components))
        } else {
            Ok(None)
        }
    }

    fn dereference_type<T: serde::de::DeserializeOwned>(
        &self,
        reference: &str,
    ) -> Result<T, OpenApiError> {
        let mut cache = self.serde_values.borrow_mut();
        let value = if let Some(v) = cache.get(reference) {
            v
        } else {
            let jp = ref_to_json_path(&reference)?;
            let query = JsonPathInst::from_str(&jp).map_err(|e| OpenApiError::ParsingError {
                msg: format!("Error creating json path {jp}, {e}"),
            })?;
            let path_result = query.find_slice(&self.json);
            //TODO Reading the spec, I don't _think_ this needs to work for arrays.
            let v = path_result.get(0).take().unwrap().deref();
            cache.insert(reference.into(), v.to_owned());
            &cache.get(reference).unwrap()
        };
        serde_json::from_value(value.clone()).map_err(|e| OpenApiError::ParsingError {
            msg: format!("Error with serde parsing {e} {reference}"),
        })
    }

    fn dereference_reference<T: serde::de::DeserializeOwned>(
        &self,
        v: ReferenceOr<T>,
    ) -> Result<ReferenceOr<T>, OpenApiError> {
        match v {
            ReferenceOr::Item(i) => Ok(ReferenceOr::Item(i)),
            ReferenceOr::Reference {
                reference,
                summary,
                description,
            } => {
                let item = self.dereference_type(&reference)?;
                Ok(ReferenceOr::DereferencedReference {
                    reference,
                    summary,
                    description,
                    item,
                })
            }
            ReferenceOr::DereferencedReference {
                reference,
                summary,
                description,
                item,
            } => Ok(ReferenceOr::DereferencedReference {
                reference,
                summary,
                description,
                item,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn is_reference<T>(reference: (&String, &ReferenceOr<T>)) -> bool {
        if let ReferenceOr::Reference {
            reference: _,
            summary: _,
            description: _,
        } = reference.1
        {
            return true;
        }
        false
    }

    #[test]
    pub fn test_ref_to_json_path() -> Result<()> {
        let reference = "#/components/parameters/pagination-before";
        let expected = "$.components.parameters.pagination-before";
        assert_eq!(expected, &ref_to_json_path(reference)?);
        Ok(())
    }

    #[test]
    pub fn test_file_ref_to_json_path() {
        let reference = "//elsewhere/components/parameters/pagination-before";
        assert!(ref_to_json_path(reference).is_err());
    }

    #[test]
    pub fn test_http_ref_to_json_path() {
        let reference = "http://mysite.com/components/parameters/pagination-before";
        assert!(ref_to_json_path(reference).is_err());
    }

    #[test]
    pub fn test_github_api_from_3_1_api() -> Result<()> {
        //NOTE: This is a sanity check. the github api doesn't have _everything_, but it
        //seems like if that passes, we're reasonably good. We might want something more
        //comprehensive in the future
        let spec = std::fs::read_to_string("oai_examples/api.github.com.json")?;
        let dereferencer = OpenApiDereferencer::from_str(&spec)?;
        let dereferenced = dereferencer.dereference()?;

        assert!(dereferenced.components.is_some());
        let components = dereferenced.components.unwrap();
        assert!(!components.security_schemes.iter().any(is_reference));
        assert!(!components.responses.iter().any(is_reference));
        assert!(!components.parameters.iter().any(is_reference));
        assert!(!components.examples.iter().any(is_reference));
        assert!(!components.request_bodies.iter().any(is_reference));
        assert!(!components.headers.iter().any(is_reference));
        assert!(!components.links.iter().any(is_reference));
        assert!(!components.callbacks.iter().any(is_reference));
        assert!(!components.path_items.iter().any(is_reference));
        Ok(())
    }

    #[test]
    pub fn test_3_0_api_is_err() -> Result<()> {
        let spec = std::fs::read_to_string("oai_examples/petstore-expanded.json")?;
        assert!(OpenApiDereferencer::from_str(&spec).is_err());
        Ok(())
    }
}
