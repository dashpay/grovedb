use crate::attribute::{ContainerAttributes, FieldAttributes};
use virtue::prelude::*;

pub(crate) struct DeriveStruct {
    pub fields: Option<Fields>,
    pub attributes: ContainerAttributes,
}

impl DeriveStruct {
    pub fn generate_encode(self, generator: &mut Generator) -> Result<()> {
        let crate_name = &self.attributes.crate_name;
        generator
            .impl_for(format!("{}::Encode", crate_name))
            .modify_generic_constraints(|generics, where_constraints| {
                if let Some((bounds, lit)) =
                    (self.attributes.encode_bounds.as_ref()).or(self.attributes.bounds.as_ref())
                {
                    where_constraints.clear();
                    where_constraints
                        .push_parsed_constraint(bounds)
                        .map_err(|e| e.with_span(lit.span()))?;
                } else {
                    for g in generics.iter_generics() {
                        where_constraints
                            .push_constraint(g, format!("{}::Encode", crate_name))
                            .unwrap();
                    }
                }
                Ok(())
            })?
            .generate_fn("encode")
            .with_generic_deps("__E", [format!("{}::enc::Encoder", crate_name)])
            .with_self_arg(virtue::generate::FnSelfArg::RefSelf)
            .with_arg("encoder", "&mut __E")
            .with_return_type(format!(
                "core::result::Result<(), {}::error::EncodeError>",
                crate_name
            ))
            .body(|fn_body| {
                if let Some(fields) = self.fields.as_ref() {
                    for field in fields.names() {
                        let attributes = field
                            .attributes()
                            .get_attribute::<FieldAttributes>()?
                            .unwrap_or_default();
                        if attributes.with_serde {
                            fn_body.push_parsed(format!(
                                "{0}::Encode::encode(&{0}::serde::Compat(&self.{1}), encoder)?;",
                                crate_name, field
                            ))?;
                        } else {
                            fn_body.push_parsed(format!(
                                "{}::Encode::encode(&self.{}, encoder)?;",
                                crate_name, field
                            ))?;
                        }
                    }
                }
                fn_body.push_parsed("core::result::Result::Ok(())")?;
                Ok(())
            })?;
        Ok(())
    }

    pub fn generate_decode(self, generator: &mut Generator) -> Result<()> {
        let decode_trait = if self.attributes.untrusted {
            "DecodeUntrusted"
        } else {
            "Decode"
        };
        let decode_method = if self.attributes.untrusted {
            "decode_untrusted"
        } else {
            "decode"
        };
        let decoder_trait = if self.attributes.untrusted {
            "UntrustedDecoder"
        } else {
            "Decoder"
        };
        // Remember to keep this mostly in sync with generate_borrow_decode
        let crate_name = &self.attributes.crate_name;
        let decode_context = if let Some((decode_context, _)) = &self.attributes.decode_context {
            decode_context.as_str()
        } else {
            "__Context"
        };

        let mut impl_for = generator.impl_for(format!("{}::{decode_trait}", crate_name));
        if self.attributes.decode_context.is_none() {
            impl_for = impl_for.with_impl_generics(["__Context"]);
        }

        impl_for
            .with_trait_generics([decode_context])
            .modify_generic_constraints(|generics, where_constraints| {
                if let Some((bounds, lit)) = (self.attributes.decode_bounds.as_ref()).or(self.attributes.bounds.as_ref()) {
                    where_constraints.clear();
                    where_constraints.push_parsed_constraint(bounds).map_err(|e| e.with_span(lit.span()))?;
                } else {
                    for g in generics.iter_generics() {
                        where_constraints.push_constraint(g, format!("{}::{decode_trait}<{}>", crate_name, decode_context)).unwrap();
                    }
                }
                Ok(())
            })?
            .generate_fn(decode_method)
            .with_generic_deps("__D", [format!("{}::de::{decoder_trait}<Context = {}>", crate_name, decode_context)])
            .with_arg("decoder", "&mut __D")
            .with_return_type(format!("core::result::Result<Self, {}::error::DecodeError>", crate_name))
            .body(|fn_body| {
                // Ok(Self {
                fn_body.push_parsed("core::result::Result::Ok")?;
                fn_body.group(Delimiter::Parenthesis, |ok_group| {
                    ok_group.ident_str("Self");
                    ok_group.group(Delimiter::Brace, |struct_body| {
                        // Fields
                        // {
                        //      a: bincode::{decode_trait}::{decode_method}(decoder)?,
                        //      b: bincode::{decode_trait}::{decode_method}(decoder)?,
                        //      ...
                        // }
                        if let Some(fields) = self.fields.as_ref() {
                            for field in fields.names() {
                                let attributes = field.attributes().get_attribute::<FieldAttributes>()?.unwrap_or_default();
                                if attributes.with_serde {
                                    struct_body
                                        .push_parsed(format!(
                                            "{1}: (<{0}::serde::Compat<_> as {0}::{decode_trait}::<{2}>>::{decode_method}(decoder)?).0,",
                                            crate_name,
                                            field,
                                            decode_context,
                                        ))?;
                                } else {
                                    struct_body
                                        .push_parsed(format!(
                                            "{1}: {0}::{decode_trait}::{decode_method}(decoder)?,",
                                            crate_name,
                                            field
                                        ))?;
                                }
                            }
                        }
                        Ok(())
                    })?;
                    Ok(())
                })?;
                Ok(())
            })?;
        self.generate_borrow_decode(generator)?;
        Ok(())
    }

    pub fn generate_borrow_decode(self, generator: &mut Generator) -> Result<()> {
        let borrow_trait = if self.attributes.untrusted {
            "BorrowDecodeUntrusted"
        } else {
            "BorrowDecode"
        };
        let borrow_method = if self.attributes.untrusted {
            "borrow_decode_untrusted"
        } else {
            "borrow_decode"
        };
        let borrow_decoder_trait = if self.attributes.untrusted {
            "BorrowUntrustedDecoder"
        } else {
            "BorrowDecoder"
        };
        // Remember to keep this mostly in sync with generate_decode
        let crate_name = self.attributes.crate_name;

        let decode_context = if let Some((decode_context, _)) = &self.attributes.decode_context {
            decode_context.as_str()
        } else {
            "__Context"
        };

        let mut impl_for = generator
            .impl_for_with_lifetimes(format!("{}::{borrow_trait}", crate_name), ["__de"])
            .with_trait_generics([decode_context]);
        if self.attributes.decode_context.is_none() {
            impl_for = impl_for.with_impl_generics(["__Context"]);
        }

        impl_for
            .modify_generic_constraints(|generics, where_constraints| {
                if let Some((bounds, lit)) = (self.attributes.borrow_decode_bounds.as_ref()).or(self.attributes.bounds.as_ref()) {
                    where_constraints.clear();
                    where_constraints.push_parsed_constraint(bounds).map_err(|e| e.with_span(lit.span()))?;
                } else {
                    for g in generics.iter_generics() {
                        where_constraints.push_constraint(g, format!("{}::de::{borrow_trait}<'__de, {}>", crate_name, decode_context)).unwrap();
                    }
                    for lt in generics.iter_lifetimes() {
                        where_constraints.push_parsed_constraint(format!("'__de: '{}", lt.ident))?;
                    }
                }
                Ok(())
            })?
            .generate_fn(borrow_method)
            .with_generic_deps("__D", [format!("{}::de::{borrow_decoder_trait}<'__de, Context = {}>", crate_name, decode_context)])
            .with_arg("decoder", "&mut __D")
            .with_return_type(format!("core::result::Result<Self, {}::error::DecodeError>", crate_name))
            .body(|fn_body| {
                // Ok(Self {
                fn_body.push_parsed("core::result::Result::Ok")?;
                fn_body.group(Delimiter::Parenthesis, |ok_group| {
                    ok_group.ident_str("Self");
                    ok_group.group(Delimiter::Brace, |struct_body| {
                        if let Some(fields) = self.fields.as_ref() {
                            for field in fields.names() {
                                let attributes = field.attributes().get_attribute::<FieldAttributes>()?.unwrap_or_default();
                                if attributes.with_serde {
                                    struct_body
                                        .push_parsed(format!(
                                            "{1}: (<{0}::serde::BorrowCompat<_> as {0}::{borrow_trait}::<'_, {2}>>::{borrow_method}(decoder)?).0,",
                                            crate_name,
                                            field,
                                            decode_context,
                                        ))?;
                                } else {
                                    struct_body
                                        .push_parsed(format!(
                                            "{1}: {0}::{borrow_trait}::<'_, {2}>::{borrow_method}(decoder)?,",
                                            crate_name,
                                            field,
                                            decode_context,
                                        ))?;
                                }
                            }
                        }
                        Ok(())
                    })?;
                    Ok(())
                })?;
                Ok(())
            })?;
        Ok(())
    }
}
